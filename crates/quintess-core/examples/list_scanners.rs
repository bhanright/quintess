fn main() { match quintess_core::scan::scanners() { Ok(list) => { println!("{} scanner(s)", list.len()); for s in list { println!("  {}", s.name); } } Err(e) => println!("error: {e:#}") } }
