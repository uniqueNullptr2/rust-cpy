use std::{path::{Path, PathBuf}, time::SystemTime, collections::{BTreeMap}, fs::{Permissions, create_dir_all}, env, sync::Arc};
use anyhow::{Result, anyhow};
use async_channel::{Receiver, Sender, unbounded};
use pathdiff::diff_paths;
use tokio::{io::{BufReader, BufWriter}, fs::File};
use walkdir::WalkDir;
use tokio_util::compat::*;
use clap::{Parser, Subcommand};
use pancurses::{Window, endwin, initscr, Input};


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
    let dst_map = Arc::new(index_path(&cli.dst)?);

    let (tx, rx) = unbounded();
    let (utx, urx) = unbounded();
    for i in 0..cli.threads {
        let t_dst_map = dst_map.clone();
        let t_cli = cli.clone();
        let trx = rx.clone();
        let tutx = utx.clone();
        tokio::spawn(async move {
            loop {
                let (path, size, permission, created, modified): (PathBuf, usize, Permissions, SystemTime, SystemTime) = trx.recv().await.unwrap();
                if let Some((dsize, _, _, _)) = t_dst_map.get(&path) {
                    if *dsize != size {
                        copy_file(i, size, path.to_owned(), t_cli.src.join(&path), t_cli.dst.join(&path), tutx.clone()).await.unwrap();
                    }
                } else {
                    copy_file(i, size, path.to_owned(), t_cli.src.join(&path), t_cli.dst.join(&path), tutx.clone()).await.unwrap();
                }
            }
            
        });
    }
    
    tokio::spawn(async move {
        for (path, (size, perm, created, modified)) in src_map.into_iter() {
            tx.send((path, size, perm, created, modified)).await.unwrap();
        }
    });

    tokio::spawn(async move {
        let mut v = vec![0u64; cli.threads as usize];
        let mut vv = vec!["".to_owned(); cli.threads as usize];
        let mut vvv = vec![0usize; cli.threads as usize];

        loop {
            let msg = urx.recv().await.unwrap();
            match msg {
                ProgressMassage::Start { id , path, size} => {
                    v[id] = 0;
                    vv[id] = path.to_string_lossy().into();
                    vvv[id] = size
                },
                ProgressMassage::Progress { id, size } => v[id] += size,
                ProgressMassage::Finished { id } =>  {
                    v[id] = vvv[id] as u64;
                },
            }
            let s: Vec<String>= (0..cli.threads as usize).map(|i| format!("{}: {}/{}",  vv[i], v[i], vvv[i])).collect();
            print!("\r{}\r", s.join(" "));
        }
    }).await?;
    Ok(())
}


// fn draw() {
//     let files = vec!("File1.pdf", "File2.bmp", "File3.xml", "File4.mov", "File5.db");
//     let window = initscr();
//     window.nodelay(true);
//     window.keypad(true);
//     noecho();
//     window.printw("Downloading Files (not Actually)\n");
//     let mut ys = window.get_cur_y();

//     let mut progressbar = ProgressBar::new(files.into_iter().map(|s| s.to_owned()).collect(), ys, 50);
//     progressbar.start();

//     ys += progressbar.len() as i32;

//     let mut items: Vec<Box<RefCell<dyn WindowItem>>> = vec!(Box::new(RefCell::new(progressbar)));
//     items.push(Box::new(RefCell::new(ScrollingMsg::new("Hello there!".to_owned(), 36, ys, 4))));

//     ys += 1;
//     progressbar = ProgressBar::new(vec!("Folder.xfp", "My Photos", "Friends Season 1").into_iter().map(|s| s.to_owned()).collect(), ys, 40);
//     progressbar.start();
//     ys += progressbar.len() as i32;
//     items.push(Box::new(RefCell::new(progressbar)));

//     items.push(Box::new(RefCell::new(ScrollingMsg::new("YAAAAAAAAAAAAS!".to_owned(), 36, ys, 4))));
//     loop {
//         match window.getch() {
//             Some(Input::KeyDC) => {
//                 break;
//             },
//             _ => (),
//         }
//         for item in &items {
//             item.borrow_mut().poll(&window);
//         }
//     }
//     endwin();
// }