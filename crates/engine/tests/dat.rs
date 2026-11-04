//! Reading the DAT formats a DAT Revision may be: Logiqx XML, and MAME's ListXML and software lists.

use std::path::Path;

use title_select_engine::dat::Dat;

#[test]
fn mame_listxml_is_the_mame_dat_versioned_by_its_build() {
    let xml = br#"<?xml version="1.0"?>
<!DOCTYPE mame [
<!ELEMENT mame (machine+)>
]>
<mame build="0.262 (mame0262)" debug="no" mameconfig="10">
  <machine name="pong" sourcefile="atari/pong.cpp">
    <description>Pong (Rev E)</description>
    <rom name="pong.a1" size="4" crc="0f0f0f0f" sha1="0123456789abcdef0123456789abcdef01234567"/>
    <rom name="nodump.b2" size="8" status="nodump"/>
  </machine>
  <machine name="z80" isdevice="yes"/>
</mame>
"#;
    let dat = Dat::parse(xml, Path::new("mame0262.xml")).unwrap();
    assert_eq!(dat.name, "MAME");
    assert_eq!(dat.version, "0.262 (mame0262)");
    assert_eq!(dat.games, ["pong", "z80"]);
    assert_eq!(dat.roms.len(), 2);
    assert_eq!(dat.roms[0].crc.as_deref(), Some("0f0f0f0f"));
}

#[test]
fn a_mame_software_list_is_named_by_its_list_and_skips_unnamed_continuation_roms() {
    let xml = br#"<?xml version="1.0"?>
<!DOCTYPE softwarelist SYSTEM "softwarelist.dtd">
<softwarelist name="nes" description="Nintendo Entertainment System cartridges">
  <software name="smb">
    <description>Super Mario Bros.</description>
    <part name="cart" interface="nes_cart">
      <dataarea name="prg" size="32768">
        <rom name="smb.prg" size="32768" crc="5cf548d3" sha1="fefe07b0b3f8bbfa86c7b2c5d4a44a3f3a2b3c4d" offset="0"/>
        <rom size="32768" offset="0x8000" loadflag="reload"/>
      </dataarea>
    </part>
  </software>
</softwarelist>
"#;
    let dat = Dat::parse(xml, Path::new("nes.xml")).unwrap();
    assert_eq!(dat.name, "nes");
    assert_eq!(dat.version, "");
    assert_eq!(dat.games, ["smb"]);
    assert_eq!(dat.roms.len(), 1);
    assert_eq!(dat.roms[0].name, "smb.prg");
}

#[test]
fn xml_that_is_no_dat_is_refused() {
    let err = Dat::parse(b"<html><body/></html>", Path::new("page.xml"))
        .err()
        .expect("not a DAT");
    assert!(err.to_string().contains("not a"), "{err}");
}
