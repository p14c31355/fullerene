#[warn(unused_imports)]
mod main_copy;
mod rotate;

#[warn(unused_imports)]
use crate::rotate::*;
use crate::main_copy::hello;

fn main() {
    pub const fn hello(){}
}
