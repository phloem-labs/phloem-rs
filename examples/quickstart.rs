use phloem::{Client, Options, Value};

fn main() -> Result<(), phloem::Error> {
    let mut c = Client::open(Options::named("hil-runner"))?;
    c.log_now("/rig/psi", Value::F64(3.2))?;
    c.close() // fails if anything was not delivered
}
