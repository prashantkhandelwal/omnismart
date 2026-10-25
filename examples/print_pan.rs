use omnismart::{ReaderContext, emv};
use std::io;

fn main() -> io::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let aid = match arguments.as_slice() {
        [] => None,
        [aid] => Some(emv::parse_aid(aid)?),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Usage: cargo run --example print_pan -- [DOCUMENTED_AID_HEX]",
            ));
        }
    };
    eprintln!("WARNING: This example prints your full PAN. Keep stdout private.");
    let context = ReaderContext::establish()?;
    let mut card = context.connect_preferred()?;
    let pan = card
        .with_transaction(|transmit| match aid.as_deref() {
            Some(aid) => emv::read_pan(transmit, aid),
            None => emv::read_pan_auto(transmit),
        })?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "PAN not provided in tag 5A"))?;
    println!("{pan}");
    Ok(())
}
