use crate::text::Span;

#[derive(Debug, thiserror::Error)]
pub enum NixError {
    #[error("unable to find 'rev' attribute in ultron input")]
    RevNotFound,

    #[error("failed to extract rev")]
    RevExtractFailed,

    #[error("flake contents are empty")]
    Empty,

    #[error("unable to build new root from updated rev")]
    BadRoot,

    #[error("unable to replace text")]
    ReplaceFailed,

    #[error(transparent)]
    SpanError(#[from] crate::text::SpanError),
}

/// get the `ultron = { ... }` block span
fn get_ultron_block<'text>(span: Span<'text>) -> Result<Span<'text>, NixError> {
    let start_keyword = "ultron = ";
    let start_pos = span.find(start_keyword)?;

    let new_span: Span<'text> = start_pos
        .get_matching_delimiters('{', '}')
        .ok_or(NixError::RevNotFound)?;

    Ok(new_span)
}

fn get_rev<'text>(span: Span<'text>) -> Result<Span<'text>, NixError> {
    let rev_keyword = "rev = ";
    let rev_pos = span.find(rev_keyword)?;

    dbg!(rev_pos.as_str());

    let rev_span = rev_pos
        .get_inner_delimiter('"')
        .ok_or(NixError::RevNotFound)?;

    Ok(rev_span)
}

fn update_ultron_rev(input: &str, new_rev: &str) -> Result<String, NixError> {
    let flake_span: Span<'_> = Span::from(input);
    if flake_span.len() == 0 {
        return Err(NixError::Empty);
    }
    let ultron_block: Span<'_> = get_ultron_block(flake_span)?;
    let rev_span: Span<'_> = get_rev(ultron_block)?;
    let updated = rev_span.replace(new_rev)?;

    Ok(updated)
}

#[cfg(test)]
mod tests {
    use tracing_test::traced_test;

    use super::*;

    const GOOD_FLAKE: &str = include_str!("../../fixtures/good_ultron.flake.nix");

    #[test]
    #[traced_test]
    fn get_ultron_current_rev_happy_path() {
        let ultron_block: Span<'_> =
            get_ultron_block(Span::from(GOOD_FLAKE)).expect("failed to get ultron block");

        insta::assert_snapshot!(ultron_block.as_str(), @r#"
        {
              type = "github";
              owner = "covercash2";
              repo = "ultron";
              rev = "0875adf8d630246ac3d0c338157a5b89fa0c57a8";
              # Make ultron use the same nixpkgs to avoid conflicts
              inputs.nixpkgs.follows = "nixpkgs";
            }
        "#);

        let rev = get_rev(ultron_block).expect("failed to get rev");

        insta::assert_snapshot!(rev.as_str(), @"0875adf8d630246ac3d0c338157a5b89fa0c57a8");
    }

    #[test]
    fn update_ultron_rev_happy_path() {
        let new_rev = "0000000000000000000000000000000000000000";

        let new_flake =
            update_ultron_rev(GOOD_FLAKE, new_rev).expect("failed to update ultron rev");

        let updated_rev: Span<'_> = get_rev(
            get_ultron_block(Span::from(new_flake.as_str())).expect("failed to get ultron block"),
        )
        .expect("failed to get rev");

        insta::assert_snapshot!(updated_rev.as_str(), @"0000000000000000000000000000000000000000");

        assert_eq!(updated_rev.as_str(), new_rev);

        assert_eq!(new_flake.len(), GOOD_FLAKE.len());
    }
}
