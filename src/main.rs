use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use clap::Parser;
use reqwest::Client;
use tokio::{io::AsyncWriteExt, process::Command, time::Instant};

#[derive(Parser, Debug)]
struct Args {
    #[arg(short, long, default_value = ".")]
    dir: PathBuf,
    #[arg(long, default_value_t = 32)]
    max_tile_gropu: i32,
    #[arg(long, short, default_value = "out.png")]
    output: PathBuf,
    #[arg(long, short)]
    verbose: bool,
    #[arg(long, short, default_value_t = 64)]
    concurrent_requests: usize,
    url: String,
}

struct State {
    args: Args,
    client: Client,
    width: tokio::sync::Mutex<i32>,
    height: tokio::sync::Mutex<i32>,
    semaphor: tokio::sync::Semaphore,
    count: tokio::sync::Mutex<i32>,
}

async fn download_file(
    s: &Arc<State>,
    x: i32,
    y: i32,
    zoom: i32,
    mut tile_group: i32,
) -> Result<i32> {
    let mut r;
    loop {
        let url = format!("{}/TileGroup{tile_group}/{zoom}-{x}-{y}.jpg", s.args.url);

        let semaphor = s.semaphor.acquire().await.unwrap();

        if s.args.verbose {
            println!("Requesting: tg{tile_group}/{zoom}-{x}-{y}",)
        }

        r = s.client.get(&url).send().await.unwrap();

        drop(semaphor);

        if s.args.verbose {
            println!(
                "Requested: tg{tile_group}/{zoom}-{x}-{y}, status:{}",
                r.status()
            )
        }

        if r.status().is_success() {
            break;
        }

        tile_group += 1;
        if tile_group > s.args.max_tile_gropu {
            anyhow::bail!("Max tile_group limit reached.\n{:?}", r);
        }
    }

    let mut file = tokio::fs::File::create(s.args.dir.join(format!("{zoom}-{x}-{y}.jpg"))).await?;

    file.write_all(&r.bytes().await?).await?;

    let mut count = s.count.lock().await;

    *count += 1;

    if *count % 10 == 0 {
        println!("Downloaded {count}/?",);
    }

    Ok(tile_group)
}

async fn download_row(s: &Arc<State>, mut x: i32, y: i32, zoom: i32, mut tile_group: i32) {
    while let Ok(tg) = download_file(s, x, y, zoom, tile_group).await {
        x += 1;
        tile_group = tg;
    }
    let mut m = s.width.lock().await;
    if x - 1 > *m {
        *m = x;
    }
}

async fn search_depth(s: &Arc<State>) {
    let start_time = Instant::now();
    let mut zoom = 0;
    let mut tile_group = 0;
    while let Ok(tg) = download_file(s, 0, 0, zoom, tile_group).await {
        tile_group = tg;
        zoom += 1;
    }

    *s.count.lock().await = 1;

    zoom -= 1;

    println!("Found highest zoom: {zoom}");

    let mut tk = Vec::new();

    let sc = s.clone();
    tk.push(tokio::spawn(async move {
        download_row(&sc, 1, 0, zoom, tile_group).await
    }));

    let mut y = 1;
    while let Ok(tg) = download_file(s, 0, y, zoom, tile_group).await {
        tile_group = tg;
        let sc = s.clone();
        tk.push(tokio::spawn(async move {
            download_row(&sc, 1, y, zoom, tile_group).await
        }));
        y += 1;
    }

    *s.height.lock().await = y;

    for j in tk {
        j.await.unwrap();
    }

    let x = *s.width.lock().await;
    let count = *s.count.lock().await;

    println!(
        "Downloaded {count}/{} ({x}x{y}) in {:.2}s",
        x * y,
        start_time.elapsed().as_secs_f32()
    );

    create_image(s, zoom).await;
}

async fn create_image(s: &Arc<State>, zoom: i32) {
    let start_time = Instant::now();
    let mut c = Command::new("magick");
    c.arg("montage");

    let width = *s.width.lock().await;
    let height = *s.height.lock().await;

    let i = (0..height)
        .map(|y| (0..width).map(move |x| (x, y)))
        .flatten()
        .map(move |(x, y)| format!("{}/{zoom}-{x}-{y}.jpg", s.args.dir.to_string_lossy()));

    c.args(i);

    c.arg("-tile");
    c.arg(format!("{}x{}", width, height));
    c.arg("-geometry");
    c.arg("256x256");
    c.arg(&s.args.output);

    let o = c
        .spawn()
        .expect("Process couldn't be spawned.")
        .wait_with_output()
        .await
        .expect("");

    if !o.stderr.is_empty() {
        println!("{}", String::from_utf8_lossy(&o.stderr));
    }

    println!(
        "Finished combining images in {:.2}",
        start_time.elapsed().as_secs_f32()
    );
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let client = Client::new();
    let state = Arc::new(State {
        semaphor: tokio::sync::Semaphore::new(args.concurrent_requests),
        args,
        client,
        width: tokio::sync::Mutex::new(0),
        height: tokio::sync::Mutex::new(0),
        count: tokio::sync::Mutex::new(0),
    });

    search_depth(&state).await;
}
