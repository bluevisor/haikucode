#!/bin/sh
# Deskbar launcher: Haiku Code in its own Terminal window, starting in the home
# folder. Installed as "apps/Haiku Code"; the binary sits in the sibling bin/.
# Terminal runs as its own team rather than exec'd: the roster pre-registers
# this script's team, and a Terminal that takes it over never finishes
# registering (it sits with one thread and no window).
here=$(dirname "$(readlink -f "$0")")
/boot/system/apps/Terminal -t "Haiku Code" -w "$HOME" "$here/../bin/haikucode" &
