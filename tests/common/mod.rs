#![allow(unused_macros)]

use tcpstate::*;

use std::io::Write;

pub mod color {
    macro_rules! ansi {
        ($name:ident, $code:literal) => {
            #[allow(unused)]
            pub fn $name() {
                print!(concat!("\u{001b}", $code));
            }
        };
    }

    ansi!(black, "[30m");
    ansi!(red, "[31m");
    ansi!(green, "[32m");
    ansi!(yellow, "[33m");
    ansi!(blue, "[34m");
    ansi!(magenta, "[35m");
    ansi!(cyan, "[36m");
    ansi!(white, "[37m");
    ansi!(default, "[38m");
    ansi!(reset, "[0m");
}

pub fn init() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("trace"))
        .format(|buf, record| {
            writeln!(
                buf,
                "[{:<15} {:>4} {:>5}] {}",
                record.module_path_static().unwrap_or("unknown"),
                record.line().unwrap_or(0),
                record.level(),
                record.args()
            )
        })
        .is_test(true)
        .try_init()
        .expect("Log init");
}

/// Forward packets from one socket to another
/// Returns true if any packets were sent
pub fn fwd(src: &mut Socket, dst: &mut Socket) -> bool {
    let mut result = false;
    let mut rounds = 0;
    while let Some(seg) = src.take_outbound() {
        dst.on_segment(seg);
        result = true;
        rounds += 1;
        assert!(
            rounds < 20,
            "Communication round limit exceeded, possible loop inner"
        );
    }
    result
}

/// Send packets between two sockets until no more traffic is generated
pub fn process(a: &mut Socket, b: &mut Socket) {
    color::green();
    println!("======== <process> ==========");
    for _ in 0..10 {
        color::magenta();
        println!("### CLIENT ###");
        let s0 = fwd(a, b);
        color::reset();
        color::yellow();
        println!("### SERVER ###");
        let s1 = fwd(b, a);
        color::reset();
        if !(s0 || s1) {
            color::green();
            print!("======== </process> =========");
            color::reset();
            println!("");
            return;
        }
    }
    color::red();
    panic!("Communication round limit exceeded, possible loop outer");
}

macro_rules! expect_retry {
    ($a:expr) => {
        match $a {
            Err(Error::RetryAfter(cookie)) => cookie,
            other => panic!("Expected a retry cookie, got {:?}", other),
        }
    };
}

macro_rules! expect_continue {
    ($a:expr) => {
        match $a {
            Err(Error::ContinueAfter(cookie)) => cookie,
            other => panic!("Expected a continue cookie, got {:?}", other),
        }
    };
}
