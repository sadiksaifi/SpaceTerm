pub(crate) use spaceterm_terminal::identity::*;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracted_engine_uses_the_same_validated_build_identity_as_the_desktop() {
        assert_eq!(PROGRAM_VERSION, env!("SPACETERM_VERSION"));
        assert_eq!(XTVERSION, concat!("SpaceTerm ", env!("SPACETERM_VERSION")));
    }
}
