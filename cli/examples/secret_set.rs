//! For tests/secrets.nix alone, and installed nowhere: TorqOS Settings' way of keeping a secret's value
//! (`torq::app::secret_set`, what its two masked fields call), without the window. The value comes on standard
//! input, and is given as both fields.
//!
//!     secret_set <name> [<person>] < value

#![forbid(unsafe_code)]
#![forbid(dead_code)]

fn main() {
    let mut args = std::env::args().skip(1);
    let (name, user) = (args.next().unwrap_or_default(), args.next());
    let mut value = String::new();
    if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut value) {
        eprintln!("reading the value: {e}");
        std::process::exit(2);
    }
    match torq::app::secret_set(&name, user.as_deref(), &value, &value) {
        Ok(said) => print!("{said}"),
        Err(e) => {
            eprintln!("{}", torq::app::said(&e));
            std::process::exit(1);
        }
    }
}
