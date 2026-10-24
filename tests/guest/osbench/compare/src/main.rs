//! `osbench-compare <run>...`: each file one osbench run of ToyOS or Linux,
//! compared per machine as Markdown on stdout (`osbench_compare`).

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        fail("usage: osbench-compare <run file>...");
    }
    let runs: Vec<osbench_compare::Run> = files
        .iter()
        .map(|file| {
            let text = std::fs::read_to_string(file).unwrap_or_else(|e| fail(&format!("{file}: {e}")));
            osbench_compare::parse(&text).unwrap_or_else(|why| fail(&format!("{file}: {why}")))
        })
        .collect();
    match osbench_compare::compare(&runs) {
        Ok(table) => print!("{table}"),
        Err(why) => fail(&why),
    }
}

fn fail(why: &str) -> ! {
    eprintln!("osbench-compare: {why}");
    std::process::exit(1);
}
