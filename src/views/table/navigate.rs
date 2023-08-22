pub(super) fn navigate(len: usize, index: usize, delta: i8) -> usize {
    let len = i32::try_from(len).expect("Directory list length fits into an i32");
    let index = i32::try_from(index).unwrap();
    let delta = i32::from(delta);
    let mut result = (index + delta) % len;
    if result < 0 {
        result += len;
    }
    usize::try_from(result).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(1,  4, 0, 1 ; "add 1")]
    #[test_case(2,  4, 0, 2 ; "add 2")]
    #[test_case(0,  4, 3, 1 ; "add 1 overflow")]
    #[test_case(1,  4, 3, 2 ; "add 2 overflow")]
    #[test_case(2,  4, 3, -1 ; "subtract 1")]
    #[test_case(1,  4, 3, -2 ; "subtract 2")]
    #[test_case(3,  4, 0, -1 ; "subtract 1 overflow")]
    #[test_case(2,  4, 0, -2 ; "subtract 2 overflow")]
    #[test_case(0,  4, 2, 10 ; "add 10 overflow")]
    #[test_case(1,  4, 2, 11 ; "add 11 overflow")]
    #[test_case(0,  4, 2, -10 ; "subtract 10 overflow")]
    #[test_case(3,  4, 2, -11 ; "subtract 11 overflow")]
    fn navigate_is_correct(expected: usize, len: usize, index: usize, delta: i8) {
        let result = navigate(len, index, delta);

        assert_eq!(expected, result);
    }
}
