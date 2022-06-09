//! Library for interacting with the kernel component of NBD, using ioctls.

#![deny(missing_docs)]

/// Wrappers for NBD ioctls.
///
/// See <https://github.com/NetworkBlockDevice/nbd/blob/master/nbd.h>.
mod nbd {
    use std::{
        fs::File,
        io,
        os::unix::prelude::{AsRawFd, RawFd},
    };

    use nix::sys::ioctl::ioctl_param_type;

    mod ioctl {
        use nix::{ioctl_none, ioctl_write_int};
        const NBD_IOCTL: u8 = 0xab;
        ioctl_write_int!(set_sock, NBD_IOCTL, 0);
        ioctl_write_int!(set_blksize, NBD_IOCTL, 1);
        ioctl_write_int!(set_size, NBD_IOCTL, 2);
        ioctl_none!(do_it, NBD_IOCTL, 3);
        ioctl_none!(clear_sock, NBD_IOCTL, 4);
        // deprecated
        // ioctl_none!(clear_que, NBD_IOCTL, 5);
        // ioctl_none!(print_debug, NBD_IOCTL, 6);
        ioctl_write_int!(set_size_blocks, NBD_IOCTL, 7);
        ioctl_none!(disconnect, NBD_IOCTL, 8);
        ioctl_write_int!(set_timeout, NBD_IOCTL, 10);
        ioctl_write_int!(set_flags, NBD_IOCTL, 10);
    }

    pub(crate) fn set_sock(f: &File, sock: RawFd) -> io::Result<()> {
        let fd = f.as_raw_fd();
        unsafe { ioctl::set_sock(fd, sock as ioctl_param_type)? };
        Ok(())
    }

    pub(crate) fn set_blksize(f: &File, blksize: u64) -> io::Result<()> {
        let fd = f.as_raw_fd();
        unsafe { ioctl::set_blksize(fd, blksize as ioctl_param_type)? };
        Ok(())
    }

    pub(crate) fn set_size_blocks(f: &File, blocks: u64) -> io::Result<()> {
        let fd = f.as_raw_fd();
        unsafe { ioctl::set_size_blocks(fd, blocks as ioctl_param_type)? };
        Ok(())
    }

    pub(crate) fn clear_sock(f: &File) -> io::Result<()> {
        let fd = f.as_raw_fd();
        unsafe { ioctl::clear_sock(fd)? };
        Ok(())
    }

    pub(crate) fn disconnect(f: &File) -> io::Result<()> {
        let fd = f.as_raw_fd();
        unsafe { ioctl::disconnect(fd)? };
        Ok(())
    }
}
