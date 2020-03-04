use sqstransfer::{parse, run};

fn main() {
    let config = parse();

    run(&config);
}
