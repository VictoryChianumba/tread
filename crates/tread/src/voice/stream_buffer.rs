use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex};

struct Inner {
  data: Vec<u8>,
  done: bool,
}

/// Reader end — implements `Read + Seek`, blocks on the condvar until data arrives.
pub struct StreamBuffer {
  shared: Arc<(Mutex<Inner>, Condvar)>,
  pos: usize,
}

/// Writer end — pushed from the network thread.
pub struct StreamWriter {
  shared: Arc<(Mutex<Inner>, Condvar)>,
}

impl StreamBuffer {
  pub fn new() -> (Self, StreamWriter) {
    let shared = Arc::new((
      Mutex::new(Inner { data: Vec::new(), done: false }),
      Condvar::new(),
    ));
    (
      StreamBuffer { shared: Arc::clone(&shared), pos: 0 },
      StreamWriter { shared },
    )
  }

  pub fn buffered_len(&self) -> usize {
    self.shared.0.lock().unwrap_or_else(|e| e.into_inner()).data.len()
  }

  pub fn is_done(&self) -> bool {
    self.shared.0.lock().unwrap_or_else(|e| e.into_inner()).done
  }
}

impl StreamWriter {
  pub fn push(&self, chunk: &[u8]) {
    let (lock, cvar) = &*self.shared;
    lock.lock().unwrap_or_else(|e| e.into_inner()).data.extend_from_slice(chunk);
    cvar.notify_all();
  }

  pub fn finish(self) {
    let (lock, cvar) = &*self.shared;
    lock.lock().unwrap_or_else(|e| e.into_inner()).done = true;
    cvar.notify_all();
  }
}

impl Read for StreamBuffer {
  fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    let (lock, cvar) = &*self.shared;
    let mut inner = lock.lock().unwrap_or_else(|e| e.into_inner());
    loop {
      let available = inner.data.len().saturating_sub(self.pos);
      if available > 0 {
        let n = available.min(buf.len());
        buf[..n].copy_from_slice(&inner.data[self.pos..self.pos + n]);
        drop(inner);
        self.pos += n;
        return Ok(n);
      }
      if inner.done {
        return Ok(0);
      }
      inner = cvar.wait(inner).unwrap_or_else(|e| e.into_inner());
    }
  }
}

impl Seek for StreamBuffer {
  fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
    let (lock, cvar) = &*self.shared;

    let (len, done) = {
      let inner = lock.lock().unwrap_or_else(|e| e.into_inner());
      (inner.data.len(), inner.done)
    };

    let new_pos: i64 = match from {
      SeekFrom::Start(n) => n as i64,
      SeekFrom::Current(n) => self.pos as i64 + n,
      SeekFrom::End(n) => {
        if done {
          len as i64 + n
        } else {
          // Symphonia asks this to detect VBR headers; return unsupported
          // so it falls back to CBR assumptions — audio still plays fine.
          return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "cannot seek from end of an incomplete stream",
          ));
        }
      }
    };

    if new_pos < 0 {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "seek before start of stream",
      ));
    }

    let new_pos = new_pos as usize;

    // Forward seek past what we have: wait on condvar until bytes arrive.
    {
      let mut inner = lock.lock().unwrap_or_else(|e| e.into_inner());
      while inner.data.len() < new_pos && !inner.done {
        inner = cvar.wait(inner).unwrap_or_else(|e| e.into_inner());
      }
    }

    self.pos = new_pos;
    Ok(self.pos as u64)
  }
}
