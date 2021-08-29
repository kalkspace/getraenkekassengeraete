use async_stream::stream;
use futures_core::stream::Stream;
use log::error;
use nix::sys::termios;
use std::convert::TryFrom;
use std::error::Error;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::time::sleep;
use tokio_fd::AsyncFd;

const STORNO: &str = "storno\n";
const STORNOEND: &str = "stornoend\n";

struct StornoFile {
    // we need to keep file in scope to read from fd
    _file: File,
    fd: AsyncFd,
}

impl StornoFile {
    pub fn new(dev: &Path) -> Result<StornoFile, Box<dyn Error>> {
        let file = File::open(dev)?;
        let fd = file.as_raw_fd();
        let mut t = termios::tcgetattr(fd)?;
        termios::cfsetispeed(&mut t, termios::BaudRate::B9600)?;
        Ok(StornoFile {
            _file: file,
            fd: AsyncFd::try_from(fd)?,
        })
    }

    pub fn fd_mut(&mut self) -> &mut AsyncFd {
        &mut self.fd
    }
}

struct StornoReader {
    dev: PathBuf,
    first_sleep_secs: Option<u64>,
    storno_file: Option<StornoFile>,
}

impl StornoReader {
    pub fn new(dev: impl Into<PathBuf>) -> StornoReader {
        StornoReader {
            dev: dev.into(),
            storno_file: None,
            first_sleep_secs: Some(0),
        }
    }

    pub async fn acquire_storno_fd(&mut self) -> &mut AsyncFd {
        if self.storno_file.is_none() {
            // special case for the very first call (try to acquire immediately)
            // if the fd somehow became None after the first call we had an error
            // and should wait before trying again
            let mut sleep_secs = self.first_sleep_secs.take().unwrap_or(1);
            while self.storno_file.is_none() {
                sleep(Duration::from_secs(sleep_secs)).await;
                self.storno_file = match StornoFile::new(&self.dev) {
                    Ok(fd) => Some(fd),
                    Err(e) => {
                        error!("Error accessing storno file {}", e);
                        if sleep_secs == 0 {
                            sleep_secs = 1;
                        } else {
                            sleep_secs = sleep_secs * 2;
                            if sleep_secs > 4 {
                                sleep_secs = 4;
                            }
                        }
                        None
                    }
                }
            }
        }

        self.storno_file.as_mut().unwrap().fd_mut()
    }

    pub async fn try_read_storno(&mut self) -> Result<(), Box<dyn Error>> {
        let fd = self.acquire_storno_fd().await;
        let mut buf = [0u8; 512];
        // sometimes the key is triggering storno and stornoend at the same time
        // check that there is some time difference between both!
        // it seems that releasing the key doesn't trigger storno
        // so the code might be stupid but works for me :S
        let mut storno = Instant::now();
        let min_storno_time = Duration::from_millis(50);
        loop {
            // this currently blocks forever even if you pull out the device. unclear how to solve that
            let r = fd.read(&mut buf).await?;
            if r == 0 {
                continue;
            }
            let st = std::str::from_utf8(&buf[0..r])?;
            if st.contains(STORNO) {
                storno = Instant::now();
            }
            if st.contains(STORNOEND) && storno.elapsed() >= min_storno_time {
                return Ok(());
            }
        }
    }

    pub async fn read_storno(&mut self) -> () {
        loop {
            match self.try_read_storno().await {
                Ok(()) => return (),
                Err(e) => {
                    error!("Error reading storno {}", e);
                    self.storno_file = None
                }
            }
        }
    }
}

pub fn run(dev: impl Into<PathBuf>) -> impl Stream<Item = ()> {
    stream! {
        let mut reader = StornoReader::new(dev);
        loop {
            yield reader.read_storno().await;
        }
    }
}
