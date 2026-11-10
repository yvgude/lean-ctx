#[derive(Debug)]
pub struct Error;

pub fn save() -> Result<(), Error> {
    Ok(())
}

pub fn parse(s: &str) -> Result<u32, Error> {
    s.parse().map_err(|_| Error)
}

pub fn run(s: &str) -> Result<u32, Error> {
    save()?;
    let n = parse(s)?;
    Ok(n)
}
