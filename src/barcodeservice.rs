use async_stream::stream;
use futures::Stream;
use libc::ioctl;
use std::convert::TryFrom;
use std::error::Error;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;
use tokio::time::{sleep, Duration};
use tokio_fd::AsyncFd;

// tja...wenn man es halt nicht kann?!
fn u8_8(u: &[u8]) -> [u8; 8] {
    [u[0], u[1], u[2], u[3], u[4], u[5], u[6], u[7]]
}

fn u8_4(u: &[u8]) -> [u8; 4] {
    [u[0], u[1], u[2], u[3]]
}

fn u8_2(u: &[u8]) -> [u8; 2] {
    [u[0], u[1]]
}

// from linux/input.h "grabs" the keyboard....i.e. keyboard input is exclusively readable by us
const EVIOCGRAB: u64 = 1074021776;

fn create_input_event(buf: &[u8]) -> libc::input_event {
    libc::input_event {
        time: libc::timeval {
            tv_sec: i64::from_le_bytes(u8_8(&buf[0..8])),
            tv_usec: i64::from_le_bytes(u8_8(&buf[8..16])),
        },
        type_: u16::from_le_bytes(u8_2(&buf[16..18])),
        code: u16::from_le_bytes(u8_2(&buf[18..20])),
        value: i32::from_le_bytes(u8_4(&buf[20..24])),
    }
}

struct KeyboardFile {
    // we need to keep file in scope to read from fd
    _file: File,
    fd: AsyncFd,
}

impl KeyboardFile {
    pub fn new(dev: &Path) -> Result<KeyboardFile, Box<dyn Error>> {
        let file = File::open(dev)?;
        let fd = file.as_raw_fd();
        // failure to understand nix ioctl wrappers...use unsafe libc ioctl directly :S
        unsafe {
            ioctl(fd, EVIOCGRAB, 1);
        }
        Ok(KeyboardFile {
            _file: file,
            fd: AsyncFd::try_from(fd)?,
        })
    }

    pub fn fd_mut(&mut self) -> &mut AsyncFd {
        &mut self.fd
    }
}

struct BarcodeScanner {
    dev: PathBuf,
    keyboard_file: Option<KeyboardFile>,
    first_sleep_secs: Option<u64>,
}

impl BarcodeScanner {
    pub fn new(dev: impl Into<PathBuf>) -> BarcodeScanner {
        BarcodeScanner {
            dev: dev.into(),
            keyboard_file: None,
            // see acquire_fd
            first_sleep_secs: Some(0),
        }
    }

    pub async fn acquire_keyboard_fd(&mut self) -> &mut AsyncFd {
        if self.keyboard_file.is_none() {
            // special case for the very first call (try to acquire immediately)
            // if the fd somehow became None after the first call we had an error
            // and should wait before trying again
            let mut sleep_secs = self.first_sleep_secs.take().unwrap_or(1);
            while self.keyboard_file.is_none() {
                sleep(Duration::from_secs(sleep_secs)).await;
                self.keyboard_file = match KeyboardFile::new(&self.dev) {
                    Ok(fd) => Some(fd),
                    Err(e) => {
                        tracing::error!("Error accessing keyboard {}", e);
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

        self.keyboard_file.as_mut().unwrap().fd_mut()
    }

    pub async fn try_read_barcode(&mut self) -> Result<String, Box<dyn Error>> {
        let input_event_size = std::mem::size_of::<libc::input_event>();
        let mut buf = [0u8; 2048];
        let mut s = String::new();

        let fd = self.acquire_keyboard_fd().await;

        loop {
            let r = fd.read(&mut buf).await?;
            // not sure if this can even happen but chunks_exact panics if r == 0
            if r == 0 {
                continue;
            }
            // chunks_exact so our buffer is always large enough to contain a full input_event
            for event_buf in buf.chunks_exact(input_event_size) {
                let event = create_input_event(&event_buf);

                if event.type_ != 1 {
                    continue;
                }
                if event.value != 0 {
                    continue;
                }
                match event.code {
                    2 => s += "1",
                    3 => s += "2",
                    4 => s += "3",
                    5 => s += "4",
                    6 => s += "5",
                    7 => s += "6",
                    8 => s += "7",
                    9 => s += "8",
                    10 => s += "9",
                    11 => s += "0",
                    28 => {
                        if s.len() > 0 {
                            return Ok(s);
                        }
                        tracing::warn!("Tried submitting empty barcode. Skipping.");
                        // ignore everything so far...expect new, clean barcode
                        s.clear();
                    }
                    _ => {
                        tracing::warn!("Invalid scancode {}", event.code);
                        s.clear();
                    }
                }
            }
        }
    }

    pub async fn read_barcode(&mut self) -> String {
        loop {
            match self.try_read_barcode().await {
                Ok(s) => return s,
                Err(e) => {
                    // todo logging
                    tracing::error!("Error reading barcode {}", e);
                    self.keyboard_file = None
                }
            }
        }
    }
}

pub fn run(dev: impl Into<PathBuf>) -> impl Stream<Item = String> {
    stream! {
        let mut scanner = BarcodeScanner::new(dev);
        loop {
            yield scanner.read_barcode().await;
        }
    }
}
