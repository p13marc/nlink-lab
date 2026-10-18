//! Reading the logs of a lab's processes: the last lines of a file
//! without loading it, a whole file as bytes, and following one as it
//! grows. The CLI's `logs`, `top` and the daemon share them.

/// Follow `path` from `start_offset`, writing each new chunk to `out`
/// until `should_continue()` returns false or an I/O error occurs.
/// Handles file truncation/rotation by reopening from offset 0 when the
/// file shrinks below the last-read position.
///
/// The CLI passes `|| true` and ends on Ctrl-C; a daemon session passes
/// its cancel token's check. Blocking: an async caller runs it in
/// `spawn_blocking`.
pub fn follow<W: std::io::Write>(
    path: &std::path::Path,
    start_offset: u64,
    out: &mut W,
    should_continue: impl Fn() -> bool,
) -> crate::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let io = |what: &str, e: std::io::Error| {
        crate::Error::Io(std::io::Error::new(
            e.kind(),
            format!("{what} log file {}: {e}", path.display()),
        ))
    };
    let mut file = std::fs::File::open(path).map_err(|e| io("open", e))?;
    file.seek(SeekFrom::Start(start_offset))
        .map_err(|e| io("seek on", e))?;
    let mut pos = start_offset;
    let mut buf = [0u8; 8192];
    while should_continue() {
        match file.read(&mut buf) {
            Ok(0) => {
                let meta = std::fs::metadata(path).ok();
                if let Some(m) = meta
                    && m.len() < pos
                {
                    file = std::fs::File::open(path).map_err(|e| io("reopen", e))?;
                    pos = 0;
                    continue;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Ok(n) => {
                out.write_all(&buf[..n]).ok();
                out.flush().ok();
                pos += n as u64;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io("read from", e)),
        }
    }
    Ok(())
}

/// Last `n` lines of a file, read backwards in 64 KiB chunks so a
/// multi-gigabyte log is never loaded into memory (#46).
///
/// Also returns the file length they were read from: where a `--follow`
/// picks up. A length taken before the read made lines written in between
/// print twice (#209).
pub fn tail_lines_at(path: &std::path::Path, n: usize) -> std::io::Result<(String, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    if n == 0 {
        return Ok((String::new(), len));
    }
    let mut end = len;
    let mut buf: Vec<u8> = Vec::new();
    const CHUNK: u64 = 64 * 1024;
    while end > 0 {
        let start = end.saturating_sub(CHUNK);
        let mut chunk = vec![0u8; (end - start) as usize];
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&buf);
        buf = chunk;
        end = start;
        // count newlines, ignoring a single trailing one
        let body = if buf.last() == Some(&b'\n') {
            &buf[..buf.len() - 1]
        } else {
            &buf[..]
        };
        if body.iter().filter(|&&b| b == b'\n').count() >= n {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    Ok((lines[start..].join("\n"), len))
}

/// Copy a whole file to `out` as bytes, in chunks: neither loaded into
/// memory nor required to be UTF-8, which failed the read (#209). Adds a
/// newline when the file does not end in one. Returns the bytes copied.
pub fn cat_to(path: &std::path::Path, out: &mut impl std::io::Write) -> std::io::Result<u64> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    let (mut total, mut last) = (0u64, None);
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        total += n as u64;
        last = Some(buf[n - 1]);
    }
    if last.is_some_and(|b| b != b'\n') {
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cat_copies_bytes_that_are_not_utf8() {
        // `logs --pid` read the file as UTF-8 and failed on this (#209)
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        std::fs::write(&p, b"caf\xe9\nno newline").unwrap();
        let mut out = Vec::new();
        assert_eq!(cat_to(&p, &mut out).unwrap(), 15);
        assert_eq!(out, b"caf\xe9\nno newline\n");
        let (tail, len) = tail_lines_at(&p, 1).unwrap();
        assert_eq!((tail.as_str(), len), ("no newline", 15));
    }

    #[test]
    fn tail_lines_reads_only_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        let body: String = (0..10_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&p, &body).unwrap();
        assert_eq!(
            tail_lines_at(&p, 3).unwrap().0,
            "line 9997\nline 9998\nline 9999"
        );
        assert_eq!(tail_lines_at(&p, 0).unwrap().0, "");
        assert_eq!(tail_lines_at(&p, 20_000).unwrap().0.lines().count(), 10_000);
        // file without trailing newline, spanning several 64 KiB chunks
        let big: String = (0..200_000).map(|i| format!("{i:08}\n")).collect();
        std::fs::write(&p, big.trim_end()).unwrap();
        assert_eq!(tail_lines_at(&p, 2).unwrap().0, "00199998\n00199999");
    }

    /// `should_continue` for the follow tests: keep polling until the
    /// writer thread reports it is done, then `extra` more polls (a poll
    /// that finds no data sleeps 250ms) so the follower sees the bytes.
    /// A fixed poll count used to double as the timeout, which made the
    /// tests depend on the writer's 100ms sleep landing inside that
    /// budget -- not a given on a starved CI runner. The 10s deadline
    /// only bounds a writer that never reports.
    fn follow_until_written(
        written: &std::sync::atomic::AtomicBool,
        extra: usize,
    ) -> impl Fn() -> bool + '_ {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let after = std::sync::atomic::AtomicUsize::new(0);
        move || {
            if written.load(std::sync::atomic::Ordering::SeqCst) {
                after.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < extra
            } else {
                std::time::Instant::now() < deadline
            }
        }
    }

    #[test]
    fn tail_follow_reads_appended_data() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("nlink-lab-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");

        std::fs::write(&path, b"initial\n").unwrap();
        let start = std::fs::metadata(&path).unwrap().len();

        // Append after a short delay from a background thread.
        let path_w = path.clone();
        let written = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let written_w = std::sync::Arc::clone(&written);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path_w)
                .unwrap();
            f.write_all(b"appended\n").unwrap();
            written_w.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        let mut out = Vec::new();
        follow(&path, start, &mut out, follow_until_written(&written, 3)).unwrap();

        let captured = String::from_utf8(out).unwrap();
        assert!(
            captured.contains("appended"),
            "expected appended content, got: {captured:?}"
        );
        assert!(
            !captured.contains("initial"),
            "should not re-read data before start_offset: {captured:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_follow_handles_truncation() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("nlink-lab-trunc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");

        std::fs::write(&path, b"old content that will be truncated\n").unwrap();
        let start = std::fs::metadata(&path).unwrap().len();

        // Truncate then write fresh content.
        let path_w = path.clone();
        let written = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let written_w = std::sync::Arc::clone(&written);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let mut f = std::fs::File::create(&path_w).unwrap(); // truncates
            f.write_all(b"fresh\n").unwrap();
            written_w.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        let mut out = Vec::new();
        follow(&path, start, &mut out, follow_until_written(&written, 4)).unwrap();

        let captured = String::from_utf8(out).unwrap();
        assert!(
            captured.contains("fresh"),
            "expected post-truncation content, got: {captured:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
