pub fn f() -> u32 {
    1
}

#[cfg(test)]
#[path = "../../../outside/x_tests.rs"]
mod tests;
