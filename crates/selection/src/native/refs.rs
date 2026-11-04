//! The message each trace reference prints, as `TraceTools.trace_title` holds them without colours. `{N}` stands
//! for the reference's Nth variable. Generated from the bundled engine.

pub(crate) fn template(reference: &str) -> &'static str {
    match reference {
        "REF0001" => "Original unmodified group:",
        "REF0002" => "Group after taking clone list priorities into account:",
        "REF0003" => {
            "Group after cleaning up preproduction/bad/pirate/mixed version-revision titles:"
        }
        "REF0004" => "Group after handling modern title rips:",
        "REF0005" => "[First pass] Group after filtering by user language order:",
        "REF0006" => "Group after handling special editions:",
        "REF0007" => "Group after handling versions and revisions:",
        "REF0008" => "Group after handling modern title rips:",
        "REF0009" => "Group after choosing dates:",
        "REF0010" => "Group after choosing good, original versions over alternatives:",
        "REF0011" => "Group after handling promotions and demotions:",
        "REF0012" => "Group after handling \"Made in\" titles:",
        "REF0013" => "Cross region parents:",
        "REF0014" => "Group after lower priority region, non-superset clones removed:",
        "REF0015" => "Highest language priority titles:",
        "REF0016" => "Top superset title for tag free name {1}: {0}",
        "REF0017" => "Top superset title/s for group:",
        "REF0018" => "Group after production title check:",
        "REF0019" => "ACTION: Clone assignments (might be reassigned later):",
        "REF0020" => "ACTION: Compare clone list priority:",
        "REF0021" => "ACTION: Compare clone list priority:",
        "REF0022" => "ACTION: Compare clone list priority (both titles are supersets):",
        "REF0023" => "ACTION: Compare clone list priority (both titles are supersets):",
        "REF0024" => "ACTION: Compare dates:",
        "REF0028" => "Compare languages\nUser language order: {0}",
        "REF0029" => "Compare languages\nUser language order: {0}",
        "REF0030" => "ACTION: Favor multiple regions higher up user region order:",
        "REF0031" => "ACTION: Favor multiple regions higher up user region order:",
        "REF0032" => "ACTION: Choose title with string:",
        "REF0033" => "ACTION: Choose title without string:",
        "REF0034" => "ACTION: Choose title with string:",
        "REF0035" => "ACTION: Choose title without string:",
        "REF0036" => "ACTION: Choose \"Made in\" title:",
        "REF0037" => "ACTION: Choose \"Made in\" title:",
        "REF0038" => "ACTION: Choose higher versions:",
        "REF0039" => "ACTION: Choose higher versions:",
        "REF0040" => "ACTION: Choose higher versions:",
        "REF0041" => "ACTION: Choose higher versions:",
        "REF0042" => "ACTION: Excluded due to categories: {0}",
        "REF0043" => "[Second pass] Group after filtering by user language order:",
        "REF0044" => "ACTION: Excluded due to a categories regex: {0}",
        "REF0045" => "ACTION: Excluded due to a system override:",
        "REF0046" => "ACTION: Excluded due to a global override:",
        "REF0047" => "ACTION: Included due to a system override:",
        "REF0048" => "ACTION: Included due to a global override:",
        "REF0049" => "ACTION: Removed due to user language filters: {0}",
        "REF0050" => {
            "Would have been a system override exclude, but was cancelled out by a system include:"
        }
        "REF0051" => {
            "Would have been a global override exclude, but was cancelled out by a global include:"
        }
        "REF0052" => {
            "Would have been a global override exclude, but was cancelled out by a system include:"
        }
        "REF0053" => "ACTION: Changing categories for title found in clone list Categories object:",
        "REF0054" => "ACTION: Removing title found in clone list Removes object:",
        "REF0055" => "ACTION: Moving title found in clone list Variants object to other group:",
        "REF0056" => {
            "ACTION: Overriding default group for title found in clone list overrides object:"
        }
        "REF0057" => "ACTION: Fail safe removal of title with lower string value:",
        "REF0058" => "ACTION: Fail safe removal of title with lower string value:",
        "REF0059" => "Group after filtering by string comparison:",
        "REF0060" => "Group after handling unlicensed/aftermarket/pirate versions:",
        "REF0061" => "Group after handling Alt versions:",
        "REF0062" => "ACTION: Excluded due to known MIA:",
        "REF0066" => "ACTION: Included due to being related to a system or global override:",
        "REF0067" => {
            "Original unmodified group: \n\nVirtual titles are marked with a :V:, the original compilation title is after the •"
        }
        "REF0068" => {
            "Group after filtering by user language order:\n\nVirtual titles are marked with a :V:, the original compilation title is after the •"
        }
        "REF0069" => {
            "Group after filtering by region order:\n\nVirtual titles are marked with a :V:, the original compilation title is after the •"
        }
        "REF0070" => "Group with original compilation names:",
        "REF0071" => "Contenders for crossover groups: {0}",
        "REF0072" => "Looking for this ideal combination: {list}\n\nFound:",
        "REF0073" => "ACTION: Compilation clone assignments",
        "REF0074" => "Only one title in this region, skipping filtering:",
        "REF0075" => "ACTION: Tie breaker, remove the compilation title:",
        "REF0076" => "Group after choosing superset titles:",
        "REF0077" => "ACTION: Choose supersets:",
        "REF0078" => "ACTION: Choose supersets:",
        "REF0079" => "Group after favoring multiple regions higher up user region order:",
        "REF0080" => "Group after filtering by clone list priority:",
        "REF0081" => "ACTION: Choose more languages:",
        "REF0082" => "ACTION: Choose more languages:",
        "REF0083" => {
            "Fallback language comparison. Compare languages based on default region order.\nDefault region language order: {0}"
        }
        "REF0084" => {
            "Fallback language comparison. Compare languages based on default region order.\nDefault region language order: {0}"
        }
        "REF0085" => "ACTION: ROM tagged as MIA due to name match in {0} (name: {1}):",
        "REF0086" => {
            "ACTION: User has region bias enabled, selecting a higher region title than the superset:"
        }
        "REF0087" => "ACTION: Keeping superset title:",
        "REF0088" => "ACTION: Keeping superset title:",
        "REF0089" => {
            "Group after filtering by user language order:\n\nVirtual titles are marked with a :V:, the original title is after the •"
        }
        "REF0090" => "ACTION: Keeping compilation that contains another:",
        "REF0091" => "ACTION: Keeping compilation that contains another:",
        "REF0092" => "Group after removing preproduction:",
        "REF0093" => "ACTION: Tie breaker, remove the compilation title:",
        "REF0094" => {
            "ACTION: Favor primary region higher up user region order (individual title vs compilation):"
        }
        "REF0095" => {
            "ACTION: Favor primary region higher up user region order (individual title vs compilation):"
        }
        "REF0096" => "Group after choosing video standard:",
        "REF0097" => {
            "Fallback language comparison. Compare languages based on region order.\nRegion language order: {0}"
        }
        "REF0098" => {
            "Fallback language comparison. Compare languages based on region order.\nRegion language order: {0}"
        }
        "REF0099" => "ACTION: Kept due to system post filter:",
        "REF0100" => "ACTION: Kept due to global post filter:",
        "REF0101" => "Group after fallback user language priority check:",
        "REF0102" => "Group after fallback higher priority region check:",
        "REF0103" => "Group after non-superset clones removed:",
        "REF0104" => "Group after fallback user language priority check:",
        "REF0105" => "ACTION: Tie breaker, keep the first compilation title:",
        "REF0106" => "ACTION: Compare clone list priority (both titles are supersets):",
        "REF0107" => "ACTION: Compare clone list priority (both titles are supersets):",
        "REF0108" => "ACTION: Changing categories for title found in clone list Variants object:",
        "REF0109" => {
            "ACTION: Moving title found in clone list Variants object to other group due to a condition being true:"
        }
        "REF0110" => {
            "ACTION: Changing a title's priority due to a condition in the Variants object being true:"
        }
        "REF0111" => {
            "ACTION: Setting a title's local name due to a condition in the Variants object being true:"
        }
        "REF0112" => {
            "ACTION: Marking a title as English-friendly due to an entry in the Variants object:"
        }
        "REF0113" => {
            "ACTION: Marking a title as English-friendly due to a condition in the Variants object being true:"
        }
        "REF0114" => {
            "ACTION: Found a superset assigned as a both a parent and clone. Setting title with the superset as a parent to the superset's parent to resolve the conflict:"
        }
        "REF0115" => "ACTION: Setting a title's local name as defined in the Variants object:",
        "REF0116" => {
            "ACTION: Setting a title as a superset due to a condition in the Variants object being true:"
        }
        "REF0117" => "ACTION: Choose title without string:",
        "REF0118" => "ACTION: Choose title without string:",
        "REF0119" => "Group after good dump/production/retail check:",
        "REF0120" => "INFO: Here are the title groups in contention:",
        "REF0121" => "INFO: Here are the regions required for each group in contention:",
        "REF0122" => "INFO: Here are the combinations in groups of {0}:",
        "REF0123" => "INFO: Here are the results of the candidate selection stages:",
        "REF0124" => "INFO: Testing these candidates for consideration:",
        "REF0125" => "INFO: Test details for candidate:",
        "REF0126" => "Group after choosing individual title due to user preference:",
        "REF0127" => "Group after choosing superset:",
        "REF0128" => "Group after comparing compilations:",
        "REF0129" => {
            "ACTION: Marking a title as the oldest due to an entry in the Variants object:"
        }
        "REF0130" => "ACTION: Compare clone list priority, oldest title:",
        "REF0131" => "ACTION: Compare clone list priority, oldest title:",
        "REF0132" => {
            "ACTION: Marking a title as the oldest due to a condition in the Variants object being true:"
        }
        "REF0133" => "Group after handling budget editions:",
        "REF0134" => "ACTION: ROM tagged as MIA due to CRC match in {0} (CRC: {1}):",
        "REF0135" => {
            "ACTION: ROM tagged as MIA due to CRC and name match in {0} (CRC: {1}, name: {2}):"
        }
        "REF0136" => "ACTION: Title tagged as supporting RetroAchievements {0}",
        "REF0137" => "ACTION: Choose RetroAchievements:",
        "REF0138" => "ACTION: Choose RetroAchievements:",
        "REF0139" => "Group after choosing RetroAchievements titles due to user preference:",
        "REF0140" => "Group after comparing World titles to USA, Europe, or Japan:",
        _ => "",
    }
}
