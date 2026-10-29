// SPDX-License-Identifier: Apache-2.0
//! The frames between an Engine and its spawn helper, and how they travel:
//! one Unix stream socket, length-prefixed frames, descriptors as `SCM_RIGHTS`
//! on the first bytes of the frame they belong to.
//!
//! Nothing here decides anything. A frame that cannot be decoded ends the
//! connection, which both sides treat as the other side being gone.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

/// A frame larger than this is not one of ours.
const MAX_FRAME: usize = 64 * 1024 * 1024;
/// Descriptors accepted with one read. A spawn request carries two.
const MAX_DESCRIPTORS: usize = 16;

#[cfg(target_vendor = "apple")]
const SEND_FLAGS: libc::c_int = 0;
#[cfg(not(target_vendor = "apple"))]
const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;

const TAG_SPAWN: u8 = 1;
const TAG_KILL: u8 = 2;
const TAG_RELEASE: u8 = 3;
const TAG_SHUTDOWN: u8 = 4;
const TAG_SPAWNED: u8 = 11;
const TAG_SPAWN_FAILED: u8 = 12;
const TAG_EXITED: u8 = 13;

/// Everything the helper needs to start one shell. The Engine has decided
/// all of it; the helper adds and removes nothing.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct SpawnSpec {
    pub(super) program: OsString,
    pub(super) args: Vec<OsString>,
    /// The complete environment of the shell, not a change to the helper's.
    pub(super) env: Vec<(OsString, OsString)>,
    pub(super) cwd: Option<OsString>,
}

/// Engine → helper.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Request {
    /// Start a shell; its output and error descriptors travel with the frame.
    Spawn { call: u64, spec: SpawnSpec },
    /// End the call's group now, while its leader is unreaped, and reap it.
    Kill { call: u64 },
    /// Let go of the call: reap its leader, never signal its group.
    Release { call: u64 },
    /// The Engine ends in order: signal nothing, exit.
    Shutdown,
}

/// Helper → Engine.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Reply {
    Spawned {
        call: u64,
        pid: u32,
    },
    /// `os_error` is the raw error of the failed start, 0 when it had none.
    SpawnFailed {
        call: u64,
        os_error: i32,
        message: String,
    },
    /// The raw wait status of the leader.
    Exited {
        call: u64,
        status: i32,
    },
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(length)?;
        self.0 = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|bytes| bytes[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let length = self.u32()? as usize;
        self.take(length)
    }

    fn os_string(&mut self) -> Option<OsString> {
        Some(OsString::from_vec(self.bytes()?.to_vec()))
    }

    fn done<T>(&self, value: T) -> Option<T> {
        self.0.is_empty().then_some(value)
    }
}

impl Request {
    pub(super) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Spawn { call, spec } => {
                out.push(TAG_SPAWN);
                put_u64(&mut out, *call);
                put_bytes(&mut out, spec.program.as_bytes());
                put_u32(&mut out, spec.args.len() as u32);
                for argument in &spec.args {
                    put_bytes(&mut out, argument.as_bytes());
                }
                put_u32(&mut out, spec.env.len() as u32);
                for (name, value) in &spec.env {
                    put_bytes(&mut out, name.as_bytes());
                    put_bytes(&mut out, value.as_bytes());
                }
                out.push(u8::from(spec.cwd.is_some()));
                if let Some(cwd) = &spec.cwd {
                    put_bytes(&mut out, cwd.as_bytes());
                }
            }
            Self::Kill { call } => {
                out.push(TAG_KILL);
                put_u64(&mut out, *call);
            }
            Self::Release { call } => {
                out.push(TAG_RELEASE);
                put_u64(&mut out, *call);
            }
            Self::Shutdown => out.push(TAG_SHUTDOWN),
        }
        out
    }

    pub(super) fn decode(frame: &[u8]) -> Option<Self> {
        let mut fields = Fields(frame);
        let request = match fields.u8()? {
            TAG_SPAWN => {
                let call = fields.u64()?;
                let program = fields.os_string()?;
                let mut args = Vec::new();
                for _ in 0..fields.u32()? {
                    args.push(fields.os_string()?);
                }
                let mut env = Vec::new();
                for _ in 0..fields.u32()? {
                    env.push((fields.os_string()?, fields.os_string()?));
                }
                let cwd = match fields.u8()? {
                    0 => None,
                    _ => Some(fields.os_string()?),
                };
                Self::Spawn {
                    call,
                    spec: SpawnSpec {
                        program,
                        args,
                        env,
                        cwd,
                    },
                }
            }
            TAG_KILL => Self::Kill {
                call: fields.u64()?,
            },
            TAG_RELEASE => Self::Release {
                call: fields.u64()?,
            },
            TAG_SHUTDOWN => Self::Shutdown,
            _ => return None,
        };
        fields.done(request)
    }
}

impl Reply {
    pub(super) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Spawned { call, pid } => {
                out.push(TAG_SPAWNED);
                put_u64(&mut out, *call);
                put_u32(&mut out, *pid);
            }
            Self::SpawnFailed {
                call,
                os_error,
                message,
            } => {
                out.push(TAG_SPAWN_FAILED);
                put_u64(&mut out, *call);
                put_u32(&mut out, *os_error as u32);
                put_bytes(&mut out, message.as_bytes());
            }
            Self::Exited { call, status } => {
                out.push(TAG_EXITED);
                put_u64(&mut out, *call);
                put_u32(&mut out, *status as u32);
            }
        }
        out
    }

    pub(super) fn decode(frame: &[u8]) -> Option<Self> {
        let mut fields = Fields(frame);
        let reply = match fields.u8()? {
            TAG_SPAWNED => Self::Spawned {
                call: fields.u64()?,
                pid: fields.u32()?,
            },
            TAG_SPAWN_FAILED => Self::SpawnFailed {
                call: fields.u64()?,
                os_error: fields.u32()? as i32,
                message: String::from_utf8_lossy(fields.bytes()?).into_owned(),
            },
            TAG_EXITED => Self::Exited {
                call: fields.u64()?,
                status: fields.u32()? as i32,
            },
            _ => return None,
        };
        fields.done(reply)
    }
}

/// Keeps a closed peer from ending this process with SIGPIPE where the
/// platform has no per-send flag for it.
pub(super) fn quiet_socket(socket: RawFd) {
    #[cfg(target_vendor = "apple")]
    {
        let on: libc::c_int = 1;
        // SAFETY: sets an integer option on a socket this process owns.
        unsafe {
            libc::setsockopt(
                socket,
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&raw const on).cast(),
                size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    let _ = socket;
}

/// Aligned room for the control data of one message.
type Control = [u64; 16];

/// How a frame failed to be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unsent {
    /// Not one byte of the frame left this process: the peer cannot act on it.
    Nothing,
    /// Some bytes left before the failure. A frame is acted on only when it
    /// is complete, but that is the peer's rule, not something seen here.
    Part,
}

/// Sends one frame; `descriptors` travel with its first bytes.
pub(super) fn send_frame(socket: RawFd, body: &[u8], descriptors: &[RawFd]) -> Result<(), Unsent> {
    let mut frame = Vec::with_capacity(body.len() + 4);
    put_u32(&mut frame, body.len() as u32);
    frame.extend_from_slice(body);
    let descriptor_bytes = size_of_val(descriptors);
    let mut control: Control = [0; 16];
    // SAFETY: pure arithmetic on a length.
    let space = unsafe { libc::CMSG_SPACE(descriptor_bytes as u32) } as usize;
    if space > size_of::<Control>() {
        return Err(Unsent::Nothing);
    }
    let mut sent = 0_usize;
    while sent < frame.len() {
        let rest = &frame[sent..];
        let mut part = libc::iovec {
            iov_base: rest.as_ptr().cast_mut().cast(),
            iov_len: rest.len(),
        };
        // SAFETY: an all-zero msghdr is valid; the fields used are set below.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &raw mut part;
        message.msg_iovlen = 1;
        if sent == 0 && !descriptors.is_empty() {
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = space as _;
            // SAFETY: the control buffer is aligned, zeroed and large enough
            // for one header and `descriptor_bytes` of data (checked above).
            unsafe {
                let header = libc::CMSG_FIRSTHDR(&raw const message);
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN(descriptor_bytes as u32) as _;
                std::ptr::copy_nonoverlapping(
                    descriptors.as_ptr().cast::<u8>(),
                    libc::CMSG_DATA(header),
                    descriptor_bytes,
                );
            }
        }
        // SAFETY: the message points at live buffers of the stated lengths.
        let written = unsafe { libc::sendmsg(socket, &raw const message, SEND_FLAGS) };
        if written >= 0 {
            sent += written as usize;
        } else {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(if sent == 0 {
                    Unsent::Nothing
                } else {
                    Unsent::Part
                });
            }
        }
    }
    Ok(())
}

/// The receiving side of one connection: bytes become frames, descriptors
/// queue up in the order they were sent.
#[derive(Default)]
pub(super) struct Inbox {
    bytes: Vec<u8>,
    descriptors: VecDeque<OwnedFd>,
}

impl Inbox {
    /// Reads once, blocking. `Ok(false)`: the peer closed the connection.
    pub(super) fn fill(&mut self, socket: RawFd) -> std::io::Result<bool> {
        let mut chunk = [0_u8; 16 * 1024];
        let mut control: Control = [0; 16];
        loop {
            let mut part = libc::iovec {
                iov_base: chunk.as_mut_ptr().cast(),
                iov_len: chunk.len(),
            };
            // SAFETY: an all-zero msghdr is valid; the fields used are set below.
            let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
            message.msg_iov = &raw mut part;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = size_of::<Control>() as _;
            // SAFETY: the message points at live buffers of the stated lengths.
            let read = unsafe { libc::recvmsg(socket, &raw mut message, 0) };
            if read < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            self.take_descriptors(&message);
            if message.msg_flags & libc::MSG_CTRUNC != 0 {
                return Err(std::io::Error::other("descriptors were cut off"));
            }
            self.bytes.extend_from_slice(&chunk[..read as usize]);
            return Ok(read > 0);
        }
    }

    fn take_descriptors(&mut self, message: &libc::msghdr) {
        // SAFETY: walks the control data the kernel wrote into our buffer;
        // every descriptor found there is new and owned by this process.
        unsafe {
            let mut header = libc::CMSG_FIRSTHDR(std::ptr::from_ref(message));
            while !header.is_null() {
                if (*header).cmsg_level == libc::SOL_SOCKET
                    && (*header).cmsg_type == libc::SCM_RIGHTS
                {
                    let data = libc::CMSG_DATA(header);
                    let bytes =
                        ((*header).cmsg_len as usize).saturating_sub(libc::CMSG_LEN(0) as usize);
                    for index in 0..(bytes / size_of::<RawFd>()).min(MAX_DESCRIPTORS) {
                        let descriptor =
                            std::ptr::read_unaligned(data.add(index * size_of::<RawFd>()).cast());
                        let flags = libc::fcntl(descriptor, libc::F_GETFD);
                        libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                        self.descriptors.push_back(OwnedFd::from_raw_fd(descriptor));
                    }
                }
                header = libc::CMSG_NXTHDR(std::ptr::from_ref(message), header);
            }
        }
    }

    /// The next complete frame. `Err`: what arrived is not a frame of ours.
    pub(super) fn next_frame(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        let Some(length) = self.bytes.first_chunk::<4>() else {
            return Ok(None);
        };
        let length = u32::from_le_bytes(*length) as usize;
        if length > MAX_FRAME {
            return Err(std::io::Error::other("not a frame"));
        }
        if self.bytes.len() < 4 + length {
            return Ok(None);
        }
        let frame = self.bytes[4..4 + length].to_vec();
        self.bytes.drain(..4 + length);
        Ok(Some(frame))
    }

    /// The oldest descriptor that has arrived and was not taken yet.
    pub(super) fn take_descriptor(&mut self) -> Option<OwnedFd> {
        self.descriptors.pop_front()
    }
}
