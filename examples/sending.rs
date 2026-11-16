use phloem::{Client, Column, GapExtent, Options, Step, TimeBasis, UniformStream, Value};

fn main() -> Result<(), phloem::Error> {
    let mut c = Client::open(Options::named("bench-pc"))?;

    // one reading at its own time, on the device's clock
    let t0: i64 = 1_700_000_000_000_000_000;
    c.log("/rig/mode", t0, TimeBasis::Source, Value::Text("fire".into()))?;

    // many readings of one channel
    c.log_many("/rig/psi", TimeBasis::Source, &[t0, t0 + 1_000_000], &[3.1, 3.2])?;

    // a fixed-rate signal: each block starts where the last one ended
    let step = Step::from_rate_hz(1000, 1).expect("a non-zero rate");
    let mut s = UniformStream::new(["/daq/ch0"], step, TimeBasis::Host)?;
    c.push(&mut s, &[Column::f64("/daq/ch0", vec![0.0; 1000])])?;

    // say that data is missing from a point
    c.gap(&["/daq/ch0".to_string()], t0, GapExtent::AtLeastSamples(12), "overrun")?;

    c.close()
}
