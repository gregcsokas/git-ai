use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use crate::daemon::trace2_events::{Trace2Event, parse_trace2_line};

// Win32 constants for named pipe creation
const PIPE_ACCESS_INBOUND: u32 = 0x00000001;
const FILE_FLAG_OVERLAPPED: u32 = 0x40000000;
const PIPE_TYPE_BYTE: u32 = 0x00000000;
const PIPE_READMODE_BYTE: u32 = 0x00000000;
const PIPE_WAIT: u32 = 0x00000000;
const PIPE_UNLIMITED_INSTANCES: u32 = 255;
const INVALID_HANDLE_VALUE: isize = -1;
const ERROR_PIPE_CONNECTED: u32 = 535;
const ERROR_IO_PENDING: u32 = 997;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;
const INFINITE: u32 = 0xFFFFFFFF;

type HANDLE = *mut std::ffi::c_void;
type BOOL = i32;
type DWORD = u32;
type LPDWORD = *mut u32;

#[repr(C)]
struct OVERLAPPED {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    h_event: HANDLE,
}

#[repr(C)]
struct SECURITY_ATTRIBUTES {
    n_length: u32,
    lp_security_descriptor: *mut std::ffi::c_void,
    b_inherit_handle: BOOL,
}

extern "system" {
    fn CreateNamedPipeW(
        lp_name: *const u16,
        dw_open_mode: DWORD,
        dw_pipe_mode: DWORD,
        n_max_instances: DWORD,
        n_out_buffer_size: DWORD,
        n_in_buffer_size: DWORD,
        n_default_time_out: DWORD,
        lp_security_attributes: *const SECURITY_ATTRIBUTES,
    ) -> HANDLE;

    fn ConnectNamedPipe(h_named_pipe: HANDLE, lp_overlapped: *mut OVERLAPPED) -> BOOL;

    fn DisconnectNamedPipe(h_named_pipe: HANDLE) -> BOOL;

    fn ReadFile(
        h_file: HANDLE,
        lp_buffer: *mut u8,
        n_number_of_bytes_to_read: DWORD,
        lp_number_of_bytes_read: LPDWORD,
        lp_overlapped: *mut OVERLAPPED,
    ) -> BOOL;

    fn CloseHandle(h_object: HANDLE) -> BOOL;

    fn CreateEventW(
        lp_event_attributes: *const SECURITY_ATTRIBUTES,
        b_manual_reset: BOOL,
        b_initial_state: BOOL,
        lp_name: *const u16,
    ) -> HANDLE;

    fn WaitForSingleObject(h_handle: HANDLE, dw_milliseconds: DWORD) -> DWORD;

    fn GetOverlappedResult(
        h_file: HANDLE,
        lp_overlapped: *mut OVERLAPPED,
        lp_number_of_bytes_transferred: LPDWORD,
        b_wait: BOOL,
    ) -> BOOL;

    fn GetLastError() -> DWORD;

    fn SetEvent(h_event: HANDLE) -> BOOL;

    fn ResetEvent(h_event: HANDLE) -> BOOL;

    fn CancelIo(h_file: HANDLE) -> BOOL;
}

/// Encode a Rust string as a null-terminated wide (UTF-16) string for Win32 APIs.
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Listens on a Windows named pipe for git trace2 events.
///
/// Git processes configured with `GIT_TRACE2_EVENT=\\.\pipe\<pipe_name>` will
/// connect to this pipe and stream newline-delimited JSON events.
pub struct Trace2ListenerWin {
    pipe_name: Vec<u16>,
    shutdown: Arc<AtomicBool>,
}

impl Trace2ListenerWin {
    /// Bind the trace2 named pipe. On Windows, named pipes don't require filesystem cleanup
    /// like Unix sockets -- they are kernel objects that disappear when all handles are closed.
    pub fn bind(pipe_name: &Path, shutdown: Arc<AtomicBool>) -> io::Result<Self> {
        let name_str = pipe_name.to_string_lossy();

        // Validate that the path looks like a named pipe path
        if !name_str.starts_with(r"\\.\pipe\") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("not a valid named pipe path: {}", name_str),
            ));
        }

        let wide_name = to_wide(&name_str);

        // Create a test pipe instance to verify we can bind this name, then close it.
        // The actual pipe instances are created per-connection in the accept loop.
        let test_handle = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                0,     // no outbound buffer needed (inbound-only pipe)
                65536, // 64KB inbound buffer
                0,     // default timeout
                std::ptr::null(),
            )
        };

        if test_handle as isize == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        // Close the test handle -- we'll create fresh instances in the accept loop
        unsafe {
            CloseHandle(test_handle);
        }

        Ok(Self {
            pipe_name: wide_name,
            shutdown,
        })
    }

    /// Run the accept loop. Spawns a thread per connection.
    ///
    /// Each connection reads newline-delimited JSON and sends parsed events to
    /// the provided channel. Returns when the shutdown flag is set.
    pub fn run(&self, event_tx: Sender<Trace2Event>) {
        while !self.shutdown.load(Ordering::Relaxed) {
            // Create a new pipe instance for this connection
            let pipe_handle = unsafe {
                CreateNamedPipeW(
                    self.pipe_name.as_ptr(),
                    PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    PIPE_UNLIMITED_INSTANCES,
                    0,
                    65536,
                    0,
                    std::ptr::null(),
                )
            };

            if pipe_handle as isize == INVALID_HANDLE_VALUE {
                eprintln!(
                    "[git-ai daemon] failed to create named pipe instance: {}",
                    io::Error::last_os_error()
                );
                thread::sleep(Duration::from_millis(500));
                continue;
            }

            // Wait for a client to connect using overlapped I/O so we can check shutdown
            match self.wait_for_connection(pipe_handle) {
                Ok(true) => {
                    // Client connected -- spawn a handler thread
                    let tx = event_tx.clone();
                    let shutdown = Arc::clone(&self.shutdown);
                    thread::spawn(move || {
                        handle_pipe_connection(pipe_handle, tx, shutdown);
                    });
                }
                Ok(false) => {
                    // Shutdown was requested while waiting
                    unsafe {
                        CloseHandle(pipe_handle);
                    }
                    break;
                }
                Err(e) => {
                    eprintln!("[git-ai daemon] ConnectNamedPipe error: {}", e);
                    unsafe {
                        CloseHandle(pipe_handle);
                    }
                    thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }

    /// Wait for a client connection with periodic shutdown checks.
    /// Returns Ok(true) if a client connected, Ok(false) if shutdown was requested.
    fn wait_for_connection(&self, pipe_handle: HANDLE) -> io::Result<bool> {
        // Create an event for overlapped ConnectNamedPipe
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }

        let mut overlapped = OVERLAPPED {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            h_event: event,
        };

        let ret = unsafe { ConnectNamedPipe(pipe_handle, &mut overlapped) };

        if ret != 0 {
            // Connected immediately (unusual but possible)
            unsafe {
                CloseHandle(event);
            }
            return Ok(true);
        }

        let err = unsafe { GetLastError() };

        if err == ERROR_PIPE_CONNECTED {
            // Client was already connected before we called ConnectNamedPipe
            unsafe {
                CloseHandle(event);
            }
            return Ok(true);
        }

        if err != ERROR_IO_PENDING {
            unsafe {
                CloseHandle(event);
            }
            return Err(io::Error::from_raw_os_error(err as i32));
        }

        // Poll the overlapped operation with a timeout, checking shutdown periodically
        loop {
            if self.shutdown.load(Ordering::Relaxed) {
                // Cancel the pending ConnectNamedPipe and clean up
                unsafe {
                    CancelIo(pipe_handle);
                    CloseHandle(event);
                }
                return Ok(false);
            }

            let wait_result = unsafe { WaitForSingleObject(event, 500) }; // 500ms timeout

            match wait_result {
                WAIT_OBJECT_0 => {
                    // Connection completed
                    let mut bytes_transferred: u32 = 0;
                    unsafe {
                        GetOverlappedResult(
                            pipe_handle,
                            &mut overlapped,
                            &mut bytes_transferred,
                            0, // don't wait
                        );
                        CloseHandle(event);
                    }
                    return Ok(true);
                }
                WAIT_TIMEOUT => {
                    // Timeout -- loop and check shutdown flag again
                    continue;
                }
                _ => {
                    // Unexpected wait result
                    unsafe {
                        CancelIo(pipe_handle);
                        CloseHandle(event);
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!("WaitForSingleObject returned {}", wait_result),
                    ));
                }
            }
        }
    }
}

/// A minimal Read adapter for a Win32 pipe handle.
///
/// Reads from the pipe using synchronous ReadFile calls (the pipe is opened
/// with FILE_FLAG_OVERLAPPED but we use blocking reads per-connection thread).
struct PipeReader {
    handle: HANDLE,
}

// SAFETY: The handle is only used by a single thread (the connection handler).
unsafe impl Send for PipeReader {}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut bytes_read: u32 = 0;
        let ret = unsafe {
            ReadFile(
                self.handle,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut bytes_read,
                std::ptr::null_mut(), // synchronous read (no overlapped)
            )
        };

        if ret == 0 {
            let err = unsafe { GetLastError() };
            // ERROR_BROKEN_PIPE (109) means client disconnected -- treat as EOF
            if err == 109 {
                return Ok(0);
            }
            return Err(io::Error::from_raw_os_error(err as i32));
        }

        Ok(bytes_read as usize)
    }
}

/// Handle a single client pipe connection, reading trace2 JSON lines until EOF or shutdown.
fn handle_pipe_connection(
    pipe_handle: HANDLE,
    event_tx: Sender<Trace2Event>,
    shutdown: Arc<AtomicBool>,
) {
    let reader = PipeReader {
        handle: pipe_handle,
    };
    let buf_reader = BufReader::new(reader);

    for line_result in buf_reader.lines() {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        match line_result {
            Ok(line) => {
                if let Some(event) = parse_trace2_line(&line) {
                    // If the receiver is gone, stop processing
                    if event_tx.send(event).is_err() {
                        break;
                    }
                }
            }
            Err(ref e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => {
                // Connection closed or broken pipe -- done with this client
                break;
            }
        }
    }

    // Disconnect and close the pipe handle
    unsafe {
        DisconnectNamedPipe(pipe_handle);
        CloseHandle(pipe_handle);
    }
}
