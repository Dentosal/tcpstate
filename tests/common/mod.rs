use tcpstate::*;

pub fn init() {
    stderrlog::new()
        .module(module_path!())
        .verbosity(4)
        .init()
        .unwrap();
}

/// Forward packets from one socket to another
/// Returns true if any packets were sent
pub fn fwd(src: &mut Socket, dst: &mut Socket) -> bool {
    let mut result = false;
    let mut rounds = 0;
    while let Some(seg) = src.take_outbound() {
        dst.on_segment(seg).expect("Error");
        result = true;
        rounds += 1;
        assert!(
            rounds < 10,
            "Communication round limit exceeded, possible loop inner"
        );
    }
    result
}

/// Send packets between two sockets until no more traffic is generated
pub fn process(a: &mut Socket, b: &mut Socket) {
    for _ in 0..10 {
        println!("SERVER => CLIENT");
        let s0 = fwd(a, b);
        println!("CLIENT => SERVER");
        let s1 = fwd(b, a);
        if !(s0 || s1) {
            return;
        }
    }
    panic!("Communication round limit exceeded, possible loop outer");
}
