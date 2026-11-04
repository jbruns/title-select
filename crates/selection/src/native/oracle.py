"""Test oracle: runs pieces of the bundled Retool on inputs a Rust test sends, and prints what they return.

Usage: python oracle.py RETOOL_DIR USER_CONFIG COMMAND < input.json
"""

import json
import pathlib
import sys

retool_dir, user_config, command = sys.argv[1], sys.argv[2], sys.argv[3]
sys.argv = [str(pathlib.Path(retool_dir) / 'retool.py')]
sys.path.insert(0, retool_dir)

import modules.constants as const  # noqa: E402
from modules.config.config import Config  # noqa: E402
from modules.input import UserInput  # noqa: E402
from modules.titletools import TitleTools  # noqa: E402

user_input = UserInput()
user_input.user_config = user_config
user_input.system_config_dir = str(pathlib.Path(user_config).parent / 'systems')
config = Config(
    const.CLONE_LIST_METADATA_DOWNLOAD_LOCATION, const.CLONE_LIST_METADATA_DOWNLOAD_LOCATION_KEY,
    const.PROGRAM_DOWNLOAD_LOCATION, const.PROGRAM_DOWNLOAD_LOCATION_KEY, const.CONFIG_FILE,
    const.DAT_FILE_TAGS_KEY, const.IGNORE_TAGS_KEY, const.DISC_RENAME_KEY, const.VERSION_IGNORE_KEY,
    const.BUDGET_EDITIONS_KEY, const.PROMOTE_EDITIONS_KEY, const.DEMOTE_EDITIONS_KEY, const.MODERN_EDITIONS_KEY,
    const.LANGUAGES_KEY, const.REGION_ORDER_KEY, const.VIDEO_ORDER_KEY, const.CLONE_LISTS_KEY, const.METADATA_KEY,
    const.MIAS_KEY, const.RA_KEY, const.USER_CONFIG_KEY, const.USER_LANGUAGE_ORDER_KEY, const.USER_REGION_ORDER_KEY,
    const.USER_LOCALIZATION_ORDER_KEY, const.USER_VIDEO_ORDER_KEY, const.USER_LIST_PREFIX_KEY,
    const.USER_LIST_SUFFIX_KEY, const.USER_OVERRIDE_EXCLUDE_KEY, const.USER_OVERRIDE_INCLUDE_KEY,
    const.USER_FILTER_KEY, const.USER_GUI_SETTINGS_KEY, const.SYSTEM_SETTINGS_PATH, const.SANITIZED_CHARACTERS,
    const.RESERVED_FILENAMES, user_input,
)


def attempt(function, *args):
    try:
        return function(*args)
    except Exception as e:  # noqa: BLE001
        return f'crash: {type(e).__name__}'


def tags_of(name):
    return {f'({x}' for x in name.split(' (')[1:]}


if command == 'titletools':
    out = []
    for name in json.load(sys.stdin):
        tags = tags_of(name)
        out.append({
            'date': attempt(TitleTools.get_date, name, config),
            'group_name': TitleTools.get_group_name(name, config),
            'versions': attempt(TitleTools.get_normalized_version, name, config),
            'short_name': TitleTools.get_short_name(name, tags, config),
            'region_free_name': TitleTools.get_region_free_name(name, tags, config),
            'languages': TitleTools.languages(name, tags, config, 'get'),
            'languages_untagged': TitleTools.languages(name, set(), config, 'get'),
            'regions': TitleTools.regions(name, config, 'get'),
            'remove_tags': TitleTools.remove_tags(name, config),
            'sanitized': TitleTools.replace_invalid_characters(name, config, False),
            'sanitized_header': TitleTools.replace_invalid_characters(name, config, True),
        })
    json.dump(out, sys.stdout, ensure_ascii=False)
