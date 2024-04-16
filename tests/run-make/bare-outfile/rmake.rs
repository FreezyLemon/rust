// Check that passing a path to `-o` that is only a filename does not
// cause an ICE. Reported in rust-lang/rust#23218.

extern crate run_make_support;

use run_make_support::Rustc;

fn main() {
    Rustc::new_bare()
        .input("foo.rs")
        .out_file("foo")
        .run();
}
