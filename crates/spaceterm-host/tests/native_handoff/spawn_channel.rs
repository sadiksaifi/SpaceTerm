//! Test-only transport observations between the public adapters and a real account spawner.

use std::{
    io::{self, Read, Write},
    mem,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::net::UnixStream,
    },
};

pub fn proxy(
    mut client: UnixStream,
    mut peer: UnixStream,
    mut observe: impl FnMut(u8, bool),
) -> io::Result<()> {
    let mut hello = [0; 16];
    peer.read_exact(&mut hello)?;
    client.write_all(&hello)?;
    loop {
        let Some(operation) = forward_request(&mut client, &mut peer)? else {
            return Ok(());
        };
        observe(operation, false);
        let reply = read_frame(&mut peer)?;
        observe(operation, true);
        write_frame(&mut client, &reply)?;
    }
}

// Delay one complete Poll frame while allowing retirement replies to pass. This models reply
// delivery independently of request processing, without replacing the real account service.
pub fn delayed_poll(
    mut client: UnixStream,
    mut peer: UnixStream,
    held: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
    armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fragment: bool,
) -> io::Result<()> {
    use std::{
        net::Shutdown,
        sync::{Arc, Mutex, mpsc},
    };
    let mut hello = [0; 16];
    peer.read_exact(&mut hello)?;
    client.write_all(&hello)?;
    let writer = Arc::new(Mutex::new(client.try_clone()?));
    let mut replies = peer.try_clone()?;
    let (sent, requests) = mpsc::channel();
    let reader = std::thread::spawn(move || -> io::Result<()> {
        let mut release = Some(release);
        let mut delayed = None;
        for operation in requests {
            let reply = read_frame(&mut replies)?;
            if operation == 2
                && armed.load(std::sync::atomic::Ordering::Acquire)
                && let Some(release) = release.take()
            {
                let writer = writer.clone();
                let held = held.clone();
                delayed = Some(std::thread::spawn(move || -> io::Result<()> {
                    if fragment {
                        let mut writer = writer.lock().unwrap();
                        let prefix = (reply.len() as u32).to_be_bytes();
                        writer.write_all(&prefix[..2])?;
                        held.send(()).unwrap();
                        release
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .unwrap();
                        writer.write_all(&prefix[2..])?;
                        writer.write_all(&reply)
                    } else {
                        held.send(()).unwrap();
                        release
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .unwrap();
                        write_frame(&mut writer.lock().unwrap(), &reply)
                    }
                }));
            } else {
                write_frame(&mut writer.lock().unwrap(), &reply)?;
            }
        }
        if let Some(delayed) = delayed {
            delayed.join().unwrap()?;
        }
        Ok(())
    });
    while let Some(operation) = forward_request(&mut client, &mut peer)? {
        sent.send(operation).unwrap();
    }
    peer.shutdown(Shutdown::Write)?;
    drop(sent);
    reader.join().unwrap()
}

fn forward_request(client: &mut UnixStream, peer: &mut UnixStream) -> io::Result<Option<u8>> {
    let mut marker = [0u8];
    let mut vector = libc::iovec {
        iov_base: marker.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 8];
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = mem::size_of_val(&control) as _;
    let count = unsafe { libc::recvmsg(client.as_raw_fd(), &mut message, 0) };
    if count == 0 {
        return Ok(None);
    }
    if count != 1 || marker != *b"S" || message.msg_flags != 0 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut descriptors = Vec::new();
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            assert_eq!((*header).cmsg_level, libc::SOL_SOCKET);
            assert_eq!((*header).cmsg_type, libc::SCM_RIGHTS);
            let length = (*header).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
            for index in 0..length / mem::size_of::<i32>() {
                let fd = libc::CMSG_DATA(header).cast::<i32>().add(index).read();
                descriptors.push(OwnedFd::from_raw_fd(fd));
                assert_eq!(libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC), 0);
            }
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    let payload = read_frame(client)?;
    let operation = payload[0];
    #[cfg(target_os = "linux")]
    let flags = libc::MSG_NOSIGNAL;
    #[cfg(target_os = "macos")]
    let flags = 0;
    let mut outgoing: libc::msghdr = unsafe { mem::zeroed() };
    outgoing.msg_iov = &mut vector;
    outgoing.msg_iovlen = 1;
    if !descriptors.is_empty() {
        outgoing.msg_control = control.as_mut_ptr().cast();
        outgoing.msg_controllen =
            unsafe { libc::CMSG_SPACE((descriptors.len() * mem::size_of::<i32>()) as _) } as _;
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&outgoing);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len =
                libc::CMSG_LEN((descriptors.len() * mem::size_of::<i32>()) as _) as _;
            for (index, descriptor) in descriptors.iter().enumerate() {
                libc::CMSG_DATA(header)
                    .cast::<i32>()
                    .add(index)
                    .write(descriptor.as_raw_fd());
            }
        }
    }
    if unsafe { libc::sendmsg(peer.as_raw_fd(), &outgoing, flags) } != 1 {
        return Err(io::Error::last_os_error());
    }
    write_frame(peer, &payload)?;
    drop(descriptors);
    Ok(Some(operation))
}

fn read_frame(channel: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut length = [0; 4];
    channel.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    assert!((1..=256 * 1024).contains(&length));
    let mut frame = vec![0; length];
    channel.read_exact(&mut frame)?;
    Ok(frame)
}

fn write_frame(channel: &mut UnixStream, frame: &[u8]) -> io::Result<()> {
    channel.write_all(&(frame.len() as u32).to_be_bytes())?;
    channel.write_all(frame)
}
