//! Retool's `Regex` class: the patterns it reads titles with, in the order its tuples list them.

use super::pyre::Re;

/// The patterns Retool reads titles with.
pub(crate) struct Regexes {
    pub alpha: Re,
    pub beta: Re,
    pub proto: Re,
    pub preprod: Re,
    pub dev: Re,
    pub not_for_resale: Re,
    pub review: Re,
    pub version: Re,
    pub long_version: Re,
    pub revision: Re,
    pub build: Re,
    pub benesse_id: Re,
    pub benesse_id_singular: Re,
    pub dreamcast_version: Re,
    pub famicom_disk_system_version: Re,
    pub fmtowns_version: Re,
    pub hyperscan_version: Re,
    pub nec_mastering_code: Re,
    pub nintendo_mastering_code: Re,
    pub nintendo_3ds_product_code: Re,
    pub ps_firmware: Re,
    pub ps1_2_id: Re,
    pub ps3_id: Re,
    pub ps3_digital_id: Re,
    pub ps4_id: Re,
    pub ps5_id: Re,
    pub psp_id: Re,
    pub ps_vita_id: Re,
    pub sega_panasonic_ring_code: Re,
    pub sega_ringedge_serial: Re,
    pub mpal_1: Re,
    pub ntsc_1: Re,
    pub ntsc_2: Re,
    pub ntsc_pal: Re,
    pub pal_1: Re,
    pub pal_2: Re,
    pub pal_60: Re,
    pub secam_1: Re,
    pub secam_2: Re,
    pub audio: Re,
    pub aftermarket: Re,
    pub alt: Re,
    pub bad: Re,
    pub bios: Re,
    pub covermount: Re,
    pub edc: Re,
    pub fmtowns_marty: Re,
    pub madein: Re,
    pub manuals: Re,
    pub multimedia: Re,
    pub oem: Re,
    pub pirate: Re,
    pub programs: Re,
    pub promotional: Re,
    pub rerelease: Re,
    pub sega32x: Re,
    pub unlicensed: Re,
    pub addons: Vec<Re>,
    pub dates: Vec<Re>,
    pub demos: Vec<Re>,
    pub video: Vec<Re>,
    /// `\(((LANGUAGES)(,\s?)?)*\)`, with the pairs of languages folded into `S(?:\+S)?`: the same strings match,
    /// and only the whole match is ever read.
    pub languages: Re,
    pub region_order_default: Re,
}

impl Regexes {
    /// `languages` are the language patterns in internal-config order; `regions` the default region order with
    /// `United Kingdom` already inserted after `UK`.
    pub fn new(languages: &[String], regions: &[String]) -> Regexes {
        let single = languages
            .iter()
            .map(|l| format!("(?:{l})"))
            .collect::<Vec<_>>()
            .join("|");
        let languages = Re::of(&format!(
            r"\((((?:{single})(?:\+(?:{single}))?|En\+En\+En)(,\s?)?)*\)"
        ));
        let mut alternatives: Vec<&str> = regions.iter().map(String::as_str).collect();
        alternatives.push("Export");
        let region_order_default = Re::of(&format!(
            r"\((?:\w*,\s)*(?:{})(?:,\s\w*)*\)",
            alternatives.join("|")
        ));
        Regexes {
            alpha: Re::of_i(r##"\((?:\w*?\s)*Alpha(?:\s\d+)?\)"##),
            beta: Re::of_i(r##"\((?:\w*?\s)*Beta(?:\s\d+)?\)"##),
            proto: Re::of_i(r##"\((?:\w*?\s)*Proto(?:type)?(?:\s\d+)?\)"##),
            preprod: Re::of_i(r##"\((?:Pre-production|Prerelease)\)"##),
            dev: Re::of_i(r##"\((?:DEV|DEBUG|Debug Build)\)"##),
            not_for_resale: Re::of_i(r##"\((?:Hibaihin|Not for Resale)\)"##),
            review: Re::of_i(r##"\(Review (Code|Kit [0-9]+)\)"##),
            version: Re::of_i(r##"\(v[\.0-9]((?!Smile).*?)\)"##),
            long_version: Re::of(
                r##"\s?(?!Version Vol\.|Version \(|Version$|Version -|Version \d-)(?: - )?\(?(?:\((?:\w[\.\-]?\s*)*|)(?:[Vv]ers(?:ion|ao)|[Vv]er\.)\s(?:[\d]+[\.\-a-zA-Z]*\)?|\s?[A-Za-z](?:$|\s|\)))+|\s[Vv]\(?(?:[\dv]+[\.\-]*)+[A-Za-z]*?\)?"##,
            ),
            revision: Re::of_i(r##"\(R[eE][vV](?:[ -][0-9A-Z].*?)?\)"##),
            build: Re::of_i(r##"\(Build [0-9].*?\)"##),
            benesse_id: Re::of(
                r##"\s\((?:[0-9][BF][BCDEFIKMPQ][0-9D]{3,3}(?:[CNOST][BCEHSW])?(?: - )?)+\)"##,
            ),
            benesse_id_singular: Re::of(
                r##"[0-9][BF][BCDEFIKMPQ][0-9D]{3,3}(?:[CNOST][BCEHSW])?"##,
            ),
            dreamcast_version: Re::of(r##"V[0-9]{2,2} L[0-9]{2,2}"##),
            famicom_disk_system_version: Re::of_i(r##"\(DV [0-9].*?\)"##),
            fmtowns_version: Re::of(
                r##"(?<!^)\s(?:V|Ver\. |- Version |Version )\d(?:\.\d+)?(?: L(?:evel )?\d+[A-Z]?)"##,
            ),
            hyperscan_version: Re::of(r##"\(USE[0-9]\)"##),
            nec_mastering_code: Re::of(r##"\((?:(?:F|S)A[A-F][ABTS](?:, )?)+\)"##),
            nintendo_mastering_code: Re::of(
                r##"\((?:A[BDEFHLNPSXY]|B[58BDFJLNPRT]|C[BX]|FT|JE|K[ADFIKMRXZ]|LB|PN|QA|RC|S[KN]|T[ABCJQ]|V[BEJKLMW]|Y[XW])[A-Z0-9][ADEJPVXYZ]\)"##,
            ),
            nintendo_3ds_product_code: Re::of(r##"\(?:[CT][TW][LR]-[NP]-[AK][7E]A[EV]\)"##),
            ps_firmware: Re::of_i(r##"\(FW[0-9].*?\)"##),
            ps1_2_id: Re::of(r##"\([LSPT][ABCDEILRSTU][ACEKPTUXZ][ACDHJLMNSX]-\d{5}\)"##),
            ps3_id: Re::of(r##"\([XBM][CLR][AEJKTU][BCDMSTVXZ]-\d{5}\)"##),
            ps3_digital_id: Re::of(r##"\([N][P][EHIJKMNOPQUVWX][A-Z]-\d{5}\)"##),
            ps4_id: Re::of(r##"\([CP][CLU][ACJKS][ABMSZ]-\d{5}\)"##),
            ps5_id: Re::of(r##"\([EP][CLP][AJS][AMS]-\d{5}\)"##),
            psp_id: Re::of(r##"\(U[CLMT][ADEJKUS][BDMPSTX]-\d{5}\)"##),
            ps_vita_id: Re::of(r##"\([PV][CLS][CAJKS][ABCDEFGHIMSX]-?\d{5}\)"##),
            sega_panasonic_ring_code: Re::of(
                r##"\((?:(?:[0-9]{1,2}[ABCMRS][0-9]?,? ?)+[B0-9]*?|R[E]?[-]?[0-9]*)\)"##,
            ),
            sega_ringedge_serial: Re::of(r##"\(DVR-\d{4,4}\)"##),
            mpal_1: Re::of(r##"(?:-)?[( ]MPAL\)?"##),
            ntsc_1: Re::of(r##"(?:-)?[( ]NTSC\)?"##),
            ntsc_2: Re::of(r##"\[(.*)?NTSC(.*)?\]"##),
            ntsc_pal: Re::of(r##"[( ]NTSC-PAL(\))?"##),
            pal_1: Re::of(r##"( -)?[( ]PAL(?: [a-zA-Z]+| 50[Hh]z)?(?:\)?| (?=\())"##),
            pal_2: Re::of(r##"\[(.*)?PAL(?!P)(.*)?\]"##),
            pal_60: Re::of(r##"\(PAL 60[Hh]z\)"##),
            secam_1: Re::of(r##"(?:-)?[( ]SECAM\)?"##),
            secam_2: Re::of(r##"\[(.*)?SECAM(.*)?\]"##),
            audio: Re::of_i(r##"\(Soundtrack\)"##),
            aftermarket: Re::of_i(r##"\(Aftermarket\)"##),
            alt: Re::of_i(r##"\(Alt.*?\)"##),
            bad: Re::of_i(r##"\[b\]"##),
            bios: Re::of_i(r##"\[BIOS\]|\(Enhancement Chip\)"##),
            covermount: Re::of_i(r##"\(Covermount\)"##),
            edc: Re::of_i(r##"\(EDC\)"##),
            fmtowns_marty: Re::of_i(r##"\(FM Towns Marty.*?\)"##),
            madein: Re::of_i(r##"\(Made in.*?\)"##),
            manuals: Re::of_i(r##"\(Manual\)"##),
            multimedia: Re::of_i(r##"\(Magazine\)"##),
            oem: Re::of_i(r##"\((?:\w-?\s*)*?OEM\)"##),
            pirate: Re::of_i(r##"\(Pirate\)"##),
            programs: Re::of_i(r##"\((?:Test )?Program\)|(Check|Sample) Program"##),
            promotional: Re::of_i(r##"EPK|Press Kit|\(Promo\)"##),
            rerelease: Re::of_i(r##"\(Rerelease\)"##),
            sega32x: Re::of_i(r##"\((?:Sega |Mega-)CD 32X\)"##),
            unlicensed: Re::of_i(r##"\(Unl\)"##),
            addons: vec![
                Re::of(r##"\((?:Addon(?: for XBLA)?|DLC)\)"##),
                Re::of(r##"\((?:Avatar|Theme)\)"##),
            ],
            dates: vec![
                Re::of(r##"\(\d{8}\)"##),
                Re::of(r##"\(\d{4}-\d{2}-\d{2}\)"##),
                Re::of(r##"\(\d{2}-\d{2}-\d{4}\)"##),
                Re::of(r##"\(\d{2}-\d{2}-\d{2}\)"##),
                Re::of(r##"\(\d{4}-\d{2}-\d{2}T\d{6}\)"##),
                Re::of(r##"\((\d{4}-\d{2})-xx\)"##),
                Re::of(r##"\(~?(\d{4})-xx-xx\)"##),
                Re::of_i(
                    r##"\((January|February|March|April|May|June|July|August|September|October|November|December),\s?\d{4}\)"##,
                ),
                Re::of(r##"\((\d{1,2}-\d{1,2}\))"##),
            ],
            demos: vec![
                Re::of_i(r##"\((?:\w[-.]?\s*)*Demo(?:(?:,?\s|-)[\w0-9\.]*)*\)"##),
                Re::of_i(r##"Cheheompan"##),
                Re::of_i(r##"Taikenban"##),
                Re::of_i(r##"\(@barai\)"##),
                Re::of_i(r##"\(GameCube Preview\)"##),
                Re::of_i(r##"\(Preview\)"##),
                Re::of_i(r##"\(Sample(?:\s[0-9]*|\s\d{4}-\d{2}-\d{2})?\)"##),
                Re::of_i(r##"Trial (Disc|Edition|Version|ver\.)"##),
                Re::of_i(r##"\((?:Full )?Trial\)"##),
                Re::of_i(
                    r##"\((?:\w-?\s*)*?Kiosk,?(?:\s\w*?)*\)|Kiosk Demo Disc|(PSP System|PS2) Kiosk"##,
                ),
            ],
            video: vec![
                Re::of_i(r##"Game Boy Advance Video"##),
                Re::of_i(r##"- (Preview|Movie) Trailer"##),
                Re::of_i(r##"\(Nintendo (?:3DS )?(?:Direct|Conference).*?\)"##),
                Re::of_i(r##"\((?:\w*\s)*Trailer(?:s|\sDisc)?(?:\s\w*)*\)"##),
                Re::of_i(r##"\((?:E3.*)?Video\)"##),
            ],
            languages,
            region_order_default,
        }
    }

    pub fn mpal(&self) -> [&Re; 1] {
        [&self.mpal_1]
    }

    pub fn ntsc(&self) -> [&Re; 3] {
        [&self.ntsc_1, &self.ntsc_2, &self.ntsc_pal]
    }

    pub fn pal(&self) -> [&Re; 3] {
        [&self.pal_1, &self.pal_2, &self.ntsc_pal]
    }

    pub fn pal_60hz(&self) -> [&Re; 1] {
        [&self.pal_60]
    }

    pub fn secam(&self) -> [&Re; 2] {
        [&self.secam_1, &self.secam_2]
    }

    pub fn preproduction(&self) -> [&Re; 5] {
        [
            &self.alpha,
            &self.beta,
            &self.proto,
            &self.preprod,
            &self.dev,
        ]
    }

    pub fn unl_group(&self) -> [&Re; 3] {
        [&self.aftermarket, &self.pirate, &self.unlicensed]
    }

    /// `config.regex[standard]` for a video standard, lowercased with spaces as underscores.
    pub fn video_standard(&self, standard: &str) -> Vec<&Re> {
        match standard {
            "mpal" => self.mpal().to_vec(),
            "ntsc" => self.ntsc().to_vec(),
            "pal" => self.pal().to_vec(),
            "pal_60hz" => self.pal_60hz().to_vec(),
            "secam" => self.secam().to_vec(),
            _ => Vec::new(),
        }
    }
}
