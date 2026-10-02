use nt_token::Sid;
use windows::core::Result;

#[test]
fn sid_from_parts_matches_parse() -> Result<()> {
    // S-1-5-32-544 → NT Authority (5) / Builtin Administrators (32-544)
    let sid_nt = Sid::from_nt_authority(&[32, 544])?;
    let sid_parse = Sid::parse("S-1-5-32-544")?;
    assert_eq!(sid_nt, sid_parse);
    assert!(sid_nt.is_valid());

    // Round-trip formatting should match the canonical string.
    assert_eq!(sid_nt.to_string()?, "S-1-5-32-544");
    Ok(())
}
