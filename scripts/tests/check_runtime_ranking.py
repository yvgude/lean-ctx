# SPDX-License-Identifier: Apache-2.0
"""Prove the retired host selection command is rejected by the public CLI."""


def check_retired_selection(run):
    run('retired-selection-command', ['select'], False,
        expected_error='engine runtime: invalid runtime installer arguments')
