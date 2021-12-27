#[macro_export]
macro_rules! expect_retry {
    ($a:expr) => {
        match $a {
            Err(Error::RetryAfter(cookie)) => cookie,
            other => panic!("Expected a retry cookie, got {:?}", other),
        }
    };
}

#[macro_export]
macro_rules! expect_continue {
    ($a:expr) => {
        match $a {
            Err(Error::ContinueAfter(cookie)) => cookie,
            other => panic!("Expected a continue cookie, got {:?}", other),
        }
    };
}
