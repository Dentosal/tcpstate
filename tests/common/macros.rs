#[macro_export]
macro_rules! expect_retry {
    ($a:expr) => {
        match $a {
            Err(Error::RetryAfter(cookie)) => cookie,
            Err(other) => panic!("Expected a retry cookie, got Err({:?})", other),
            Ok(_) => panic!("Expected a retry cookie, got ok"),
        }
    };
}

#[macro_export]
macro_rules! expect_continue {
    ($a:expr) => {
        match $a {
            Err(Error::ContinueAfter(cookie)) => cookie,
            Err(other) => panic!("Expected a continue cookie, got Err({:?})", other),
            Ok(_) => panic!("Expected a continue cookie, got ok"),
        }
    };
}
