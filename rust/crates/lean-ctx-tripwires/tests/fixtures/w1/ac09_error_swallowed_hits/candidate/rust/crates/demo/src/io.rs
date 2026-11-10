#[derive(Debug)]
pub struct Error;

pub fn save() -> Result<(), Error> {
    Ok(())
}

pub fn parse(s: &str) -> Result<u32, Error> {
    s.parse().map_err(|_| Error)
}

pub fn run(s: &str) -> Result<u32, Error> {
    let _ = save();
    save().ok();
    match save() {
        Ok(()) => {}
        Err(_) => {}
    }
    let _ = std::panic::catch_unwind(|| parse(s));
    let n = parse(s).unwrap_or_default();
    Ok(n)
}
