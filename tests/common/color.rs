use std::sync::atomic::{AtomicUsize, Ordering};

pub mod code {
    //! See https://gist.github.com/fnky/458719343aabd01cfb17a3a4f7296797

    macro_rules! ansi {
        ($code:literal) => {
            concat!("\u{001b}", $code)
        };
    }

    pub const BLACK: &str = ansi!("[30m");
    pub const RED: &str = ansi!("[31m");
    pub const GREEN: &str = ansi!("[32m");
    pub const YELLOW: &str = ansi!("[33m");
    pub const BLUE: &str = ansi!("[34m");
    pub const MAGENTA: &str = ansi!("[35m");
    pub const CYAN: &str = ansi!("[36m");
    pub const WHITE: &str = ansi!("[37m");

    pub const LOG_COLOR_1: &str = ansi!("[38;5;99m");
    pub const LOG_COLOR_2: &str = ansi!("[38;5;154m");
    pub const LOG_COLOR_3: &str = ansi!("[38;5;211m");

    pub const DEFAULT: &str = ansi!("[38m");
    pub const RESET: &str = ansi!("[0m");
}

macro_rules! short {
    ($name:ident, $code:expr) => {
        #[allow(unused)]
        pub fn $name() {
            print!("{}", $code);
        }
    };
}

short!(black, code::BLACK);
short!(red, code::RED);
short!(green, code::GREEN);
short!(yellow, code::YELLOW);
short!(blue, code::BLUE);
short!(magenta, code::MAGENTA);
short!(cyan, code::CYAN);
short!(white, code::WHITE);
short!(default, code::DEFAULT);
short!(reset, code::RESET);

static NEXT_COLOR: AtomicUsize = AtomicUsize::new(0);

const COLORS: &[&str] = &[
    code::GREEN,
    code::YELLOW,
    code::CYAN,
    code::MAGENTA,
    code::LOG_COLOR_1,
    code::LOG_COLOR_2,
    code::LOG_COLOR_3,
    code::RED,
];

pub fn next() -> &'static str {
    let index = NEXT_COLOR.fetch_add(1, Ordering::SeqCst);
    COLORS[index % COLORS.len()]
}
