use std::io::Read;
use std::cmp::PartialEq;
use std::fs::File;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;
use postcard::accumulator::CobsAccumulator;
use postcard::from_bytes_cobs;
use postcard::ser_flavors::Cobs;
use serde::{Deserialize, Serialize};
use serde_big_array::Array;
use serialport::SerialPort;
use structopt::StructOpt;
use crate::SerialFlasherCommand::WritePage;

type RelativeAddress = usize;
type Length = u32;
type Page = Array<u8, 256>;
type Crc8Checksum = u8;
type Sha256Checksum = [u8; 16];

#[derive(Debug, StructOpt)]
#[structopt(name = "my_program", about = "A CLI application example")]
struct Opt {
    /// Input file
    #[structopt(parse(from_os_str))]
    input: PathBuf,

    #[structopt(long, default_value = "268451840")]
    offset: u32,

    /// Serial port path
    #[structopt(long, conflicts_with = "tcp", required_unless = "tcp")]
    port: Option<PathBuf>,

    /// TCP address in the format IP:PORT
    #[structopt(long, conflicts_with = "port", required_unless = "port")]
    tcp: Option<SocketAddr>,
}


const CRC8: crc::Crc<u8> = crc::Crc::<u8>::new(&crc::CRC_8_SMBUS);

#[derive(Serialize, Debug)]
enum SerialFlasherCommand {
    Hello,
    PrepareForUpdate,
    WritePage(RelativeAddress, Page, Crc8Checksum),
    FinishedWriting,
    CompareChecksum(Length, Sha256Checksum),
    MarkUpdated
}

#[derive(Deserialize, Debug, PartialEq)]
enum SerialFlasherResponse {
    Ack,
    Nack,
}

#[derive(Debug)]
enum FlasherError {
    NoResponse,
    CouldntDeserialize,
}

fn main() {
    let opt = Opt::from_args();

    println!("Input file: {:?}", opt.input);

    let mut file = File::open(opt.input).expect("File needs to be able to open");
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).expect("Have to be able to read the file");

    let blocks = buffer.chunks_exact(512);
    let write_commands = blocks
        .map(|chunk| uftwo::Block::from_bytes(chunk).expect("Gotta be able to parse chunk"))
        .map(|b| {
            if opt.offset > b.target_addr {
                return None;
            }

            let relative_address =  b.target_addr - opt.offset;

            if b.data_len != 256 {
                panic!("Non-256 length block");
            }

            let mut data = Array::<u8, 256>::default();
            data.copy_from_slice(&b.data[..256]);

            let checksum = CRC8.checksum(&b.data[..256]);

            let page_write = WritePage(relative_address as usize, data, checksum);

            Some(page_write)
        })
        .flatten();

    let port = opt.port.expect("Only serial port is allowed right now");
    let port =  port.to_str().unwrap_or("Invalid UTF-8 path");;
    let port = "/dev/cu.usbserial-110";

    let mut port = serialport::new(port, 9600)
        .timeout(Duration::from_millis(10000))
        .open().expect("Failed to open port");

    send_command(SerialFlasherCommand::Hello, &mut port).unwrap();
    let resp = send_command(SerialFlasherCommand::PrepareForUpdate, &mut port).unwrap();
    if resp == SerialFlasherResponse::Ack {
        for command in write_commands {
            let resp = send_command(command, &mut port).unwrap();

            if resp == SerialFlasherResponse::Nack {
                panic!("We received a NACK in response to a write. No bueno!");
            }
        }
        let r = send_command(SerialFlasherCommand::FinishedWriting, &mut port).unwrap();
        if r == SerialFlasherResponse::Ack {
            send_command(SerialFlasherCommand::MarkUpdated, &mut port).unwrap();
        }
    }
}

fn send_command(cmd: SerialFlasherCommand, port: &mut Box<dyn SerialPort>) -> Result<SerialFlasherResponse, FlasherError> {
    println!("Sending {:?}", cmd);
    let ser = postcard::to_stdvec_cobs(&cmd).expect("Failed to serialize");

    println!("Serialized: {:?}", ser);

    let chunks = ser.chunks(16);

    for chunk in chunks {
        println!("Writing chunk");
        port.write(chunk).expect("Write failed!");
        sleep(Duration::from_millis(1));
    }

    let mut serial_buf: Vec<u8> = vec![0; 32];
    let res = port.read(serial_buf.as_mut_slice());

    if let Ok(len) = res {
        let resp = from_bytes_cobs::<SerialFlasherResponse>(&mut serial_buf[..len]);
        if let Ok(resp) = resp {
            println!("Received response: {:?}", resp);
            return Ok(resp);
        } else {
            println!("Couldn't deserialize response: {:?}", resp.unwrap_err());
            return Err(FlasherError::CouldntDeserialize);
        }
    } else {
        println!("Didn't read a response");
        return Err(FlasherError::NoResponse);
    }

}