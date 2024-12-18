//! Basic NBD client that works with this crate's server.
//!
//! See the documentation for [`Client`].
#![deny(missing_docs)]

use color_eyre::eyre::bail;
use color_eyre::Result;

use std::{
    io::prelude::*,
    net::TcpStream,
    os::unix::io::{IntoRawFd, RawFd},
};

use byteorder::{ReadBytesExt, WriteBytesExt, BE};

use crate::proto::*;

#[derive(Debug)]
struct Export {
    size: u64,
}

/// Client provides an interface to an export from a remote NBD server.
#[derive(Debug)]
pub struct Client<IO: Read + Write> {
    conn: IO,
    export: Export,
}

impl<IO: Read + Write> Client<IO> {
    fn initial_handshake(stream: &mut (impl Read + Write)) -> Result<()> {
        let magic = stream.read_u64::<BE>()?;
        if magic != MAGIC {
            bail!(ProtocolError::new(format!("unexpected magic {}", magic)));
        }
        let opt_magic = stream.read_u64::<BE>()?;
        if opt_magic != IHAVEOPT {
            bail!(ProtocolError::new(format!(
                "unexpected IHAVEOPT value {opt_magic}",
            )))
        }
        let server_flags = stream.read_u16::<BE>()?;
        let server_flags = HandshakeFlags::from_bits(server_flags)
            .ok_or_else(|| ProtocolError::new(format!("unexpected server flags {server_flags}")))?;
        if !server_flags.contains(HandshakeFlags::FIXED_NEWSTYLE | HandshakeFlags::NO_ZEROES) {
            bail!(ProtocolError::new("server does not support NO_ZEROES"));
        }
        let client_flags =
            ClientHandshakeFlags::C_FIXED_NEWSTYLE | ClientHandshakeFlags::C_NO_ZEROES;
        stream.write_u32::<BE>(client_flags.bits())?;
        Ok(())
    }

    fn get_export_info(stream: &mut impl Read) -> Result<(Export, TransmitFlags)> {
        let size = stream.read_u64::<BE>()?;
        let transmit_flags = stream.read_u16::<BE>()?;
        let transmit_flags = TransmitFlags::from_bits(transmit_flags)
            .ok_or_else(|| ProtocolError::new("invalid transmit flags {transmit_flags}"))?;
        let export = Export { size };
        Ok((export, transmit_flags))
    }

    fn handshake_haggle(stream: &mut (impl Read + Write)) -> Result<Export> {
        Opt {
            typ: OptType::EXPORT_NAME,
            data: b"default".to_vec(),
        }
        .put(stream)?;
        // ignore transmit flags for now (we don't send anything fancy anyway)
        let (export, _transmit_flags) = Self::get_export_info(stream)?;
        Ok(export)
    }

    /// Establish a handshake with stream and return a `Client` ready for use.
    pub fn new(mut stream: IO) -> Result<Self> {
        Self::initial_handshake(&mut stream)?;
        let export = Self::handshake_haggle(&mut stream)?;
        Ok(Self {
            conn: stream,
            export,
        })
    }

    /// Return the size of this export, as reported by the server during the
    /// handshake.
    pub fn size(&self) -> u64 {
        self.export.size
    }

    fn get_reply_data(&mut self, req: &Request, buf: &mut [u8]) -> Result<()> {
        let reply = SimpleReply::get(&mut self.conn, buf)?;
        if reply.handle != req.handle {
            bail!(format!(
                "reply for wrong handle {} != {}",
                reply.handle, req.handle
            ))
        }
        if reply.err != ErrorType::OK {
            bail!(format!("{:?} failed: {:?}", req.typ, reply.err))
        }
        Ok(())
    }

    fn get_ack(&mut self, req: &Request) -> Result<()> {
        self.get_reply_data(req, &mut [])
    }

    /// Send a read command to the NBD server.
    pub fn read(&mut self, offset: u64, len: u32) -> Result<Vec<u8>> {
        let req = Request::new(Cmd::READ, offset, len);
        req.put(&[], &mut self.conn)?;
        let mut buf = vec![0; len as usize];
        self.get_reply_data(&req, &mut buf)?;
        Ok(buf)
    }

    /// Send a write command to the NBD server.
    pub fn write(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let req = Request::new(Cmd::WRITE, offset, data.len() as u32);
        req.put(data, &mut self.conn)?;
        self.get_ack(&req)?;
        Ok(())
    }

    /// Send a flush command to the NBD server.
    pub fn flush(&mut self) -> Result<()> {
        let req = Request::new(Cmd::FLUSH, 0, 0);
        req.put(&[], &mut self.conn)?;
        self.get_ack(&req)?;
        Ok(())
    }

    /// Disconnect from server cleanly and consume this client.
    pub fn disconnect(mut self) -> Result<()> {
        Request::new(Cmd::DISCONNECT, 0, 0).put(&[], &mut self.conn)?;
        Ok(())
    }
}

impl Client<TcpStream> {
    /// Connect to a server, run handshake, and return a `Client` prepared for
    /// the transmission phase.
    pub fn connect(host: &str, port: u16) -> Result<Self> {
        let stream = TcpStream::connect((host, port))?;
        Self::new(stream)
    }
}

impl<IO: Read + Write + IntoRawFd> IntoRawFd for Client<IO> {
    fn into_raw_fd(self) -> RawFd {
        self.conn.into_raw_fd()
    }
}
