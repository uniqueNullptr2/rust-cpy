use std::{path::{Path, PathBuf}, time::{SystemTime, Duration}, collections::{BTreeMap}, fs::{Permissions, create_dir_all}, env, sync::Arc};
use anyhow::{Result, anyhow};
use async_channel::{Receiver, Sender, unbounded};
use pathdiff::diff_paths;
use tokio::{io::{BufReader, BufWriter}, fs::File, time::Instant};
use walkdir::WalkDir;
use tokio_util::compat::*;
use clap::{Parser, Subcommand};
use pancurses::{Window, endwin, initscr, Input, noecho};


#[derive(Parser, Clone)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Optional name to operate on
    src: PathBuf,
    dst: PathBuf,

    /// Turn debugging information on
    #[arg(short, long, default_value_t = 5)]
    threads: usize,
}

#[derive(Subcommand)]
enum Commands {
    /// does testing things
    Test {
        /// lists test values
        #[arg(short, long)]
        list: bool,
    },
}

enum ProgressMassage {
    Start{id: usize, path: PathBuf, size: usize},
    Progress{id: usize, size: u64},
    Finished{id: usize}
}


fn index_path<T: AsRef<Path>>(root: T) -> Result<BTreeMap<PathBuf, (usize, Permissions, SystemTime, SystemTime)>> {
    let rootp = root.as_ref().to_owned();
    let mut map = BTreeMap::<PathBuf, (usize, Permissions, SystemTime, SystemTime)>::new();
    if !rootp.exists() {
        return Ok(map);
    }
    for entry in WalkDir::new(&rootp) {
        let e = entry?;
        if e.file_type().is_file() {
            // TODO own pathdiff without cloning
            let diff = diff_paths(e.path().to_owned(), rootp.clone()).unwrap();
            let meta = e.metadata()?;
            map.insert(diff, (meta.len() as usize, meta.permissions(), meta.created()?, meta.modified()?));
        }
    }
    Ok(map)
}

async fn copy_file(id: usize, size: usize, rel: PathBuf, src: PathBuf, dst: PathBuf, send: Sender<ProgressMassage>) -> Result<()> {
    if !dst.exists() {
        create_dir_all(dst.parent().ok_or(anyhow!("No parent"))?)?;
    }
    send.send(ProgressMassage::Start{id, path: rel.clone(), size}).await?;
    let srcf = File::open(src).await?;
    let dstf = File::create(dst).await?;
    let reader = BufReader::new(srcf);
    let writer = BufWriter::new(dstf);
    let report_progress = |amt| {
        send.send_blocking(ProgressMassage::Progress { id, size: amt }).unwrap();
    };
    async_copy_progress::copy(&mut reader.compat(), &mut writer.compat_write(), report_progress).await?;
    send.send(ProgressMassage::Finished{id}).await?;
    Ok(())
}


#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    println!("Indexing");

    let src_map = index_path(&cli.src)?;
    let dst_map = index_path(&cli.dst)?;
    let mut files = 0;
    let mut t_size = 0;

    for (_, (si, _, _, _)) in src_map.iter().filter(|(k, _)| !dst_map.contains_key(*k)) {
        files += 1;
        t_size += si;
    }

    let (tx, rx) = unbounded();
    let (utx, urx) = unbounded();
    for i in 0..cli.threads {
        // let t_dst_map = dst_map.clone();
        let t_cli = cli.clone();
        let trx = rx.clone();
        let tutx = utx.clone();
        tokio::spawn(async move {
            loop {
                let (path, size, permission, created, modified): (PathBuf, usize, Permissions, SystemTime, SystemTime) = trx.recv().await.unwrap();
                copy_file(i, size, path.to_owned(), t_cli.src.join(&path), t_cli.dst.join(&path), tutx.clone()).await.unwrap();
            }
            
        });
    }
    
    tokio::spawn(async move {
        for (path, (size, perm, created, modified)) in src_map.into_iter() {
            if let Some((dsize, _, _, _)) = dst_map.get(&path) {
                if *dsize != size {
                    tx.send((path, size, perm, created, modified)).await.unwrap();
                }
            } else {
                tx.send((path, size, perm, created, modified)).await.unwrap();
            }
        }
    });

    let window = initscr();
    window.nodelay(true);
    window.keypad(true);
    noecho();
    let start_y = window.get_cur_y();

    let mut v = vec![0u64; cli.threads as usize];
    let mut vv = vec!["".to_owned(); cli.threads as usize];
    let mut vvv = vec![0usize; cli.threads as usize];

    let mut count = 0;
    let mut c_size: usize = 0;
    let mut last = Instant::now() - Duration::from_millis(64);
    loop {
        if count == files {
            break;
        }
        let res = urx.recv().await;
        if let Ok(msg) = res {
            match msg {
                ProgressMassage::Start { id , path, size} => {
                    v[id] = 0;
                    vv[id] = path.file_name().unwrap().to_str().unwrap().to_owned();
                    vvv[id] = size;
                },
                ProgressMassage::Progress { id, size } => {
                    c_size = c_size - v[id] as usize + size as usize;
                    v[id] = size;
                },
                ProgressMassage::Finished { id } =>  {
                    // v[id] = vvv[id] as u64;
                    count += 1;
                },
            }
            if (Instant::now() - last).as_millis() < 20 {
                continue
            } else {
                draw(&window, start_y, files, count, t_size, c_size, &vv, &v, &vvv).await.unwrap();
                last = Instant::now();
            }
        } else {
            break;
        }
    }
    endwin();
    Ok(())
}


async fn draw(win: &Window, start_y: i32, total_files: usize, files: usize, total_size: usize, size: usize, file_names: &Vec<String>, file_current_size: &Vec<u64>, file_max_size: &Vec<usize>) -> Result<()> {

    let (factor, s) = get_size_factor(total_size);

    win.erase();
    win.mv(start_y as i32, win.get_beg_x());
    let f_width = (total_files as f64).log10().floor() as i32 + 1;
    let width = win.get_max_x() - 28 - 2*s.len() as i32 - 2 * f_width;

    let prog  = format!("{:>5.2}{} / {:>5.2}{} - {:width$} / {:width$}", size as f64 / factor, s, total_size as f64 / factor, s, files, total_files, width = f_width as usize);

    let bar = get_progress_bar(width as usize, size as f64, total_size as f64);
    let output = format!("{} {}", prog, bar );
    win.printw(output);

    let l = file_names.iter().map(|n| n.len()).max().unwrap();
    for i in 0..file_names.len() {
        win.mv(win.get_cur_y()+1 , win.get_beg_x());
        
        let name = &file_names[i];
        let file_progress = file_current_size[i];
        let file_size = file_max_size[i];
        let (factor, s) = get_size_factor(file_max_size[i]);

        let width = win.get_max_x() - 30 - 2*s.len() as i32 - l as i32;
        let prog  = format!("{:>6.2}{} / {:>6.2}{} {:width$}", file_progress as f64 / factor, s, file_size as f64 / factor, s, name, width = l);
        let bar = get_progress_bar(width as usize, file_progress as f64, file_size as f64);
        
        let output = format!("{} {}", prog, bar );
        win.printw(output);
    }
    
    win.refresh();

    
    Ok(())
}

fn get_size_factor(size: usize) -> (f64, String) {
    if size < 1024 {
        (1.0, "b".to_owned())
    } else if size < 1024 * 1024 {
        (1024.0, "Kb".to_owned())
    } else if size < 1024 * 1024 * 1024 {
        ((1024.0*1024.0), "Mb".to_owned())
    } else if size < 1024 * 1024 * 1024 * 1024 {
        ((1024.0*1024.0*1024.0), "Gb".to_owned())
    } else {
        ((1024.0*1024.0*1024.0*1024.0), "Tb".to_owned())
    }
}

fn get_progress_bar(width: usize, current: f64, total: f64) -> String {
    let progress = current / total;
    let w = width - 8;
    let str = if progress == 100.0 {
        "=".repeat(w)
    } else {
        let x = (progress * (w - 1) as f64).floor() as usize;
        ["=".repeat(x), ">".to_owned(), " ".repeat(w.saturating_sub(x))].join("")
    };
    let s = format!("{:.2}", progress*100.0);
    format!("{:>7}%% [{}]",s, str )
}
