//! Network Block Device server, exporting an underlying file.
//!
//! Implements the most basic parts of the protocol: a single export,
//! read/write/flush commands, and no other flags (eg, read-only or TLS
//! support).
//!
//! See <https://github.com/NetworkBlockDevice/nbd/blob/master/doc/proto.md> for
//! the protocol description.

#![deny(missing_docs)]
use color_eyre::eyre::{bail, WrapErr};
use color_eyre::Result;

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, prelude::*};
use std::net::TcpListener;
use std::os::unix::fs::FileExt;

use byteorder::{ReadBytesExt, WriteBytesExt, BE};
use log::{info, warn};

use crate::proto::*;

/// Blocks is a byte array that can be exported by this server, with a basic
/// read/write API that works on arbitrary offsets.
///
/// Blocks is implemented for unix files (using the underlying `pread` and
/// `pwrite` system calls) and for `RefCell<[u8]>` for exporting an in-memory
/// byte array.
pub trait Blocks {
    /// Fill buf starting from off (reading `buf.len()` bytes)
    fn read_at(&self, buf: &mut [u8], off: u64) -> io::Result<()>;

    /// Write data from buf to self starting at off (writing `buf.len()` bytes)
    fn write_at(&self, buf: &[u8], off: u64) -> io::Result<()>;

    /// Get the size of this array (in bytes)
    fn size(&self) -> io::Result<u64>;

    /// Flush any outstanding writes to stable storage.
    fn flush(&self) -> io::Result<()>;
}

impl Blocks for File {
    fn read_at(&self, buf: &mut [u8], off: u64) -> io::Result<()> {
        FileExt::read_exact_at(self, buf, off)
    }

    fn write_at(&self, buf: &[u8], off: u64) -> io::Result<()> {
        FileExt::write_all_at(self, buf, off)
    }

    fn size(&self) -> io::Result<u64> {
        self.metadata().map(|m| m.len())
    }

    fn flush(&self) -> io::Result<()> {
        self.sync_all()?;
        Ok(())
    }
}

/// MemBlocks is a convenience for an in-memory implementation of Blocks using
/// an array of bytes.
type MemBlocks = RefCell<Vec<u8>>;

impl Blocks for MemBlocks {
    fn read_at(&self, buf: &mut [u8], off: u64) -> io::Result<()> {
        let off = off as usize;
        if off + buf.len() >= self.borrow().len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "out-of-bounds read",
            ));
        }
        let data = self.borrow();
        buf.copy_from_slice(&data[off..off + buf.len()]);
        Ok(())
    }

    fn write_at(&self, buf: &[u8], off: u64) -> io::Result<()> {
        let off = off as usize;
        if off + buf.len() >= self.borrow().len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "out-of-bounds write",
            ));
        }
        let mut data = self.borrow_mut();
        data[off..off + buf.len()].copy_from_slice(buf);
        Ok(())
    }

    fn size(&self) -> io::Result<u64> {
        Ok(self.borrow().len() as u64)
    }

    fn flush(&self) -> io::Result<()> {
        Ok(())
    }
}

/// A file to be exported as a block device.
#[derive(Debug)]
pub struct Export<F: Blocks> {
    /// name of the export (only used for listing)
    pub name: String,
    /// file to be exported
    pub file: F,
}

impl<F: Blocks> Export<F> {
    fn read<'a, 'b>(
        &'a self,
        off: u64,
        len: u32,
        buf: &'b mut [u8],
    ) -> core::result::Result<&'b mut [u8], ErrorType> {
        let len = len as usize;
        if buf.len() < len {
            return Err(ErrorType::EOVERFLOW);
        }
        let buf = &mut buf[..len];
        match Blocks::read_at(&self.file, buf, off) {
            Ok(_) => Ok(buf),
            Err(err) => Err(ErrorType::from_io_kind(err.kind())),
        }
    }

    fn write(&self, off: u64, len: usize, data: &[u8]) -> core::result::Result<(), ErrorType> {
        if len > data.len() {
            return Err(ErrorType::EOVERFLOW);
        }
        let data = &data[..len];
        Blocks::write_at(&self.file, data, off)
            .map_err(|err| ErrorType::from_io_kind(err.kind()))?;
        Ok(())
    }

    fn flush(&self) -> io::Result<()> {
        self.file.flush()?;
        Ok(())
    }

    fn size(&self) -> io::Result<u64> {
        self.file.size().map(|s| s as u64)
    }
}

/// Server implements the NBD protocol, with a single export.
#[derive(Debug)]
pub struct Server<F: Blocks> {
    export: Export<F>,
}

impl<F: Blocks> Server<F> {
    // fake constant for the server's supported operations
    #[allow(non_snake_case)]
    fn TRANSMIT_FLAGS() -> TransmitFlags {
        TransmitFlags::HAS_FLAGS | TransmitFlags::SEND_FLUSH
    }

    /// Create a Server for export
    pub fn new(export: Export<F>) -> Self {
        Self { export }
    }

    // agree on basic negotiation flags (only fixed newstyle is supported so
    // this returns a unit)
    fn initial_handshake<IO: Read + Write>(stream: &mut IO) -> Result<HandshakeFlags> {
        stream.write_u64::<BE>(MAGIC)?;
        stream.write_u64::<BE>(IHAVEOPT)?;
        stream
            .write_u16::<BE>((HandshakeFlags::FIXED_NEWSTYLE | HandshakeFlags::NO_ZEROES).bits())?;
        let client_flags = stream.read_u32::<BE>()?;
        let client_flags = ClientHandshakeFlags::from_bits(client_flags)
            .ok_or_else(|| ProtocolError::new(format!("unexpected client flags {client_flags}")))?;
        if !client_flags.contains(ClientHandshakeFlags::C_FIXED_NEWSTYLE) {
            bail!(ProtocolError::new("client does not support FIXED_NEWSTYLE"));
        }
        let mut flags = HandshakeFlags::FIXED_NEWSTYLE;
        if client_flags.contains(ClientHandshakeFlags::C_NO_ZEROES) {
            flags |= HandshakeFlags::NO_ZEROES;
        }
        Ok(flags)
    }

    fn send_export_list<IO: Write>(&self, stream: &mut IO) -> Result<()> {
        ExportList::new(vec![self.export.name.clone()]).put(stream)?;
        Ok(())
    }

    /// send export info at the end of newstyle negotiation, when client sends NBD_OPT_EXPORT_NAME
    fn send_export_info<IO: Write>(&self, stream: &mut IO, flags: HandshakeFlags) -> Result<()> {
        // If the value of the option field is `NBD_OPT_EXPORT_NAME` and the
        // server is willing to allow the export, the server replies with
        // information about the used export:
        //
        // S: 64 bits, size of the export in bytes (unsigned)
        // S: 16 bits, transmission flags
        // S: 124 bytes, zeroes (reserved) (unless `NBD_FLAG_C_NO_ZEROES` was negotiated by the client)
        stream.write_u64::<BE>(self.export.size()?)?;
        let transmit = Self::TRANSMIT_FLAGS();
        stream.write_u16::<BE>(transmit.bits())?;
        if !flags.contains(HandshakeFlags::NO_ZEROES) {
            stream.write_all(&[0u8; 124])?;
        }
        stream.flush()?;
        Ok(())
    }

    fn info_responses<IO: Write>(
        &self,
        opt_typ: OptType,
        info_req: InfoRequest,
        stream: &mut IO,
    ) -> Result<()> {
        for typ in info_req.typs.iter().chain([InfoType::EXPORT].iter()) {
            match typ {
                InfoType::EXPORT => {
                    // Mandatory information before a successful completion of
                    // NBD_OPT_INFO or NBD_OPT_GO. Describes the same
                    // information that is sent in response to the older
                    // NBD_OPT_EXPORT_NAME, except that there are no trailing
                    // zeroes whether or not NBD_FLAG_C_NO_ZEROES was
                    // negotiated. length MUST be 12, and the reply payload is
                    // interpreted as follows:
                    //
                    // - 16 bits, NBD_INFO_EXPORT
                    // - 64 bits, size of the export in bytes (unsigned)
                    // - 16 bits, transmission flags
                    let mut buf = vec![];
                    buf.write_u16::<BE>(InfoType::EXPORT.into())?;
                    buf.write_u64::<BE>(self.export.size()? as u64)?;
                    buf.write_u16::<BE>(Self::TRANSMIT_FLAGS().bits())?;
                    OptReply::new(opt_typ, ReplyType::INFO, buf).put(stream)?;
                }
                InfoType::BLOCK_SIZE => {
                    // Represents the server's advertised block size
                    // constraints; see the "Block size constraints" section for
                    // more details on what these values represent, and on
                    // constraints on their values. The server MUST send this
                    // info if it is requested and it intends to enforce block
                    // size constraints other than the defaults. After sending
                    // this information in response to an NBD_OPT_GO in which
                    // the client specifically requested NBD_INFO_BLOCK_SIZE,
                    // the server can legitimately assume that any client that
                    // continues the session will support the block size
                    // constraints supplied (note that this assumption cannot be
                    // made solely on the basis of an NBD_OPT_INFO with an
                    // NBD_INFO_BLOCK_SIZE request, or an NBD_OPT_GO without an
                    // explicit NBD_INFO_BLOCK_SIZE request). The length MUST be
                    // 14, and the reply payload is interpreted as:
                    //
                    //  -  16 bits, NBD_INFO_BLOCK_SIZE
                    //  -  32 bits, minimum block size
                    //  -  32 bits, preferred block size
                    //  -  32 bits, maximum block size

                    let mut buf = vec![];
                    buf.write_u16::<BE>(InfoType::BLOCK_SIZE.into())?;
                    buf.write_u32::<BE>(1)?; // minimum
                    buf.write_u32::<BE>(4096)?; // preferred
                    buf.write_u32::<BE>(4096 * 32)?; // maximum
                    OptReply::new(opt_typ, ReplyType::INFO, buf).put(stream)?;
                }
                InfoType::NAME | InfoType::DESCRIPTION => {
                    OptReply::new(opt_typ, ReplyType::ERR_UNSUP, vec![]).put(stream)?;
                    return Ok(());
                }
            }
        }
        OptReply::ack(opt_typ).put(stream)?;
        Ok(())
    }

    /// After the initial handshake, "haggle" to agree on connection parameters.
    //
    /// If this returns Ok(None), then the client wants to disconnect
    fn handshake_haggle<IO: Read + Write>(
        &self,
        stream: &mut IO,
        flags: HandshakeFlags,
    ) -> Result<Option<&Export<F>>> {
        loop {
            let opt = Opt::get(stream)?;
            match opt.typ {
                OptType::EXPORT_NAME => {
                    let _export: String = String::from_utf8(opt.data)
                        .wrap_err(ProtocolError::new("non-UTF8 export name"))?;
                    // requested export name is currently ignored since there is
                    // only a single export
                    self.send_export_info(stream, flags)?;
                    return Ok(Some(&self.export));
                }
                OptType::LIST => {
                    self.send_export_list(stream)?;
                }
                // the only difference between INFO and GO is that on success,
                // GO starts the transmission phase
                OptType::INFO => {
                    let info_req = InfoRequest::get(&mut &opt.data[..])?;
                    self.info_responses(opt.typ, info_req, stream)?;
                }
                OptType::GO => {
                    let info_req = InfoRequest::get(&mut &opt.data[..])?;
                    self.info_responses(opt.typ, info_req, stream)?;
                    return Ok(Some(&self.export));
                }
                OptType::ABORT => {
                    return Ok(None);
                }
                _ => {
                    warn!("got unsupported option {:?}", opt);
                    OptReply::new(opt.typ, ReplyType::ERR_UNSUP, vec![]).put(stream)?;
                }
            }
        }
    }

    fn handle_ops<IO: Read + Write>(export: &Export<F>, stream: &mut IO) -> Result<()> {
        let mut buf = vec![0u8; 4096 * 64];
        loop {
            assert_eq!(buf.len(), 4096 * 64);
            let req = match Request::get(stream, &mut buf)? {
                Some(req) => req,
                None => {
                    // client closed connection and disconnected
                    return Ok(());
                }
            };
            info!(target: "nbd", "{:?}", req);
            match req.typ {
                Cmd::READ => match export.read(req.offset, req.len, &mut buf) {
                    Ok(data) => SimpleReply::data(&req, data).put(stream)?,
                    Err(err) => {
                        warn!(target: "nbd", "read error {:?}", err);
                        SimpleReply::err(err, &req).put(stream)?;
                    }
                },
                Cmd::WRITE => match export.write(req.offset, req.data_len, &buf) {
                    Ok(_) => SimpleReply::ok(&req).put(stream)?,
                    Err(err) => {
                        warn!(target: "nbd", "write error {:?}", err);
                        SimpleReply::err(err, &req).put(stream)?;
                    }
                },
                Cmd::DISCONNECT => {
                    // don't send a reply - RFC says server can send an ACK, but Linux client closes the connection immediately
                    return Ok(());
                }
                Cmd::FLUSH => {
                    export.flush()?;
                    SimpleReply::ok(&req).put(stream)?;
                }
                Cmd::TRIM => {
                    SimpleReply::ok(&req).put(stream)?;
                }
                _ => {
                    SimpleReply::err(ErrorType::ENOTSUP, &req).put(stream)?;
                    return Ok(());
                }
            }
        }
    }

    /// Handshake and communicate with a client on a single connection.
    ///
    /// Returns Ok(()) when client gracefully disconnects.
    pub fn handle_client<IO: Read + Write>(&self, mut stream: IO) -> Result<()> {
        let flags = Self::initial_handshake(&mut stream).wrap_err("initial handshake failed")?;
        info!("handshake with {:?}", flags);
        if let Some(export) = self
            .handshake_haggle(&mut stream, flags)
            .wrap_err("handshake haggling failed")?
        {
            info!("handshake finished");
            Server::handle_ops(export, &mut stream).wrap_err("handling client operations")?;
        }
        Ok(())
    }

    /// Start accepting connections from clients and processing commands.
    ///
    /// Currently accepts in a single thread, so only one client can be
    /// connected at a time.
    pub fn start(self) -> Result<()> {
        let addr = ("127.0.0.1", TCP_PORT);
        let listener = TcpListener::bind(addr)?;
        for stream in listener.incoming() {
            let stream = stream?;
            stream.set_nodelay(true)?;
            info!(target: "nbd", "client connected");
            // TODO: how to process clients in parallel? self has to be shared among threads
            match self.handle_client(stream) {
                Ok(_) => info!(target: "nbd", "client disconnected"),
                Err(err) => eprintln!("error handling client:\n{:?}", err),
            }
        }
        Ok(())
    }
}
