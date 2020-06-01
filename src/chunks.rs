use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

use anyhow::Context;

/// Create an Iterator over chunks of lines of a file.
///
/// The last line in a chunk potentially reads over the chunk byte boundary to find the line end.
/// In the same way the first line searches the line end.
pub fn chunks(path: PathBuf) -> anyhow::Result<Chunks<FileChunks>> {
    let size = File::open(&path)?.metadata()?.len();
    let cpus = num_cpus::get() as u64;
    let chunk_size = MAX_CHUNK_SIZE.min(size / cpus / 10).max(MIN_CHUNK_SIZE);
    Ok(Chunks::new(FileChunks { path }, size, chunk_size))
}

pub struct Chunks<S: ChunkSource> {
    source: S,
    position: u64,
    count: usize,
    chunks: usize,
    chunk_size: u64,
    size: u64,
}

impl<S: ChunkSource> Chunks<S> {
    fn new(source: S, size: u64, chunk_size: u64) -> Self {
        let chunks = if chunk_size == 0 {
            0
        } else {
            size / chunk_size + 1.min(size % chunk_size)
        } as usize;

        Chunks {
            source,
            position: 0,
            count: 0,
            chunks,
            chunk_size,
            size,
        }
    }
}

impl<S: ChunkSource> Iterator for Chunks<S> {
    type Item = Chunk<S::Item>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.count == self.chunks {
            return None;
        }

        let start = (self.count as u64) * self.chunk_size;
        self.count += 1;
        let f = self.source.call().ok()?;
        let (chunk, position) =
            Chunk::new(f, self.chunk_size, self.position, start, self.size).ok()?;
        self.position = position;
        Some(chunk)
    }
}

#[derive(Debug)]
pub struct Chunk<C> {
    lines: std::io::Lines<C>,
    position: u64,
    end: u64,
}

impl<C> Chunk<C> {
    pub fn new(
        mut chunk: C,
        chunk_size: u64,
        mut position: u64,
        start: u64,
        size: u64,
    ) -> anyhow::Result<(Self, u64)>
    where
        C: Seek + BufRead,
    {
        let skip = if position > start {
            true
        } else if start != position {
            chunk.seek(SeekFrom::Start(start - 1))?;
            let mut buf = [0 as u8; 1];
            if let Ok(1) = chunk.read(&mut buf) {
                buf[0] != b'\n'
            } else {
                false
            }
        } else {
            false
        };

        chunk.seek(SeekFrom::Start(start))?;
        position = if skip {
            let mut skip_leader = String::new();
            let _ = chunk.read_line(&mut skip_leader)?;
            start + skip_leader.len() as u64
        } else {
            start
        };
        let lines = chunk.lines();
        let c = Self {
            lines,
            position,
            end: size.min(start + chunk_size),
        };
        Ok((c, position))
    }
}

impl<T> Iterator for Chunk<T>
where
    T: Seek + BufRead,
{
    type Item = String;
    fn next(&mut self) -> Option<String> {
        if self.position >= self.end {
            return None;
        }
        let l = self.lines.next()?.ok()?;
        self.position += l.len() as u64 + 1;
        Some(l)
    }
}

pub trait ChunkSource: Sized {
    type Item: Seek + BufRead;

    fn call(&self) -> anyhow::Result<Self::Item>;
}

pub struct FileChunks {
    path: PathBuf,
}

impl ChunkSource for FileChunks {
    type Item = BufReader<File>;

    fn call(&self) -> anyhow::Result<Self::Item> {
        File::open(&self.path)
            .map(BufReader::new)
            .with_context(|| "Failed")
    }
}

const MIN_CHUNK_SIZE: u64 = 512 * 1024;
const MAX_CHUNK_SIZE: u64 = 64 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck::TestResult;
    use std::io::Cursor;

    #[test]
    fn test_chunks() {
        fn test_split_buf(i: Vec<String>, chunk_size: u64) -> TestResult {
            fn t(b: String, chunk_size: u64) -> anyhow::Result<()> {
                let bytes = b.as_bytes().to_owned();
                let size = bytes.len() as u64;
                let source = MemoryChunks { bytes };

                let chunks: Vec<_> = Chunks::new(source, size, chunk_size)
                    .into_iter()
                    .map(|i| i.collect::<Vec<_>>().join("\n"))
                    .collect();

                let r = regex::Regex::new(r"\s+")?;
                let e = r.replace_all(&b, " ");
                let cs = chunks.join(" ");
                let a = r.replace_all(&cs, " ");
                if e.trim() == a.trim() {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!(
                        "Expected >{}< Actual >{}<",
                        e.trim(),
                        a.trim()
                    ))
                }
            }

            let filter = regex::Regex::new(r"\W").unwrap();
            if chunk_size < 1 || i.iter().any(|s| filter.is_match(&s)) {
                TestResult::discard()
            } else if let Err(e) = t(i.join("\n"), chunk_size) {
                TestResult::error(format!("{:?}", e))
            } else {
                TestResult::from_bool(true)
            }
        }
        quickcheck::QuickCheck::new()
            .max_tests(300)
            .quickcheck(test_split_buf as fn(_, _) -> TestResult);
    }

    struct MemoryChunks {
        bytes: Vec<u8>,
    }

    impl ChunkSource for MemoryChunks {
        type Item = Cursor<Vec<u8>>;

        fn call(&self) -> anyhow::Result<Self::Item> {
            Ok(Cursor::new(self.bytes.clone()))
        }
    }
}
