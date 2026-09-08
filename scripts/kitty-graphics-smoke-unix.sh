#!/bin/sh
set -eu

# Repeatable direct-media smoke test for SpaceTerm's static Kitty graphics path.
printf '\033[2J\033[H'
printf 'SpaceTerm static Kitty graphics smoke test\r\n'
printf 'RGBA alpha, z=-1 (text should remain visible):\r\n'
# The trailing backslash is Kitty's ST terminator, interpreted by printf.
# shellcheck disable=SC1003
printf '\033_Ga=T,t=d,f=32,i=8901,p=1,s=2,v=2,c=20,r=8,C=1,z=-1,q=1;/wAA/wD/AMgAAP+g//8AeA==\033\\'
printf 'TEXT OVER IMAGE\r\n'
printf '\033[12;1HPNG, z=0 (image paints above text):\r\n'
# shellcheck disable=SC1003
printf '\033_Ga=T,t=d,f=100,i=8902,p=2,c=10,r=5,C=1,z=0,q=1;iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==\033\\'
printf 'covered by PNG\r\n'
printf '\033[19;1HResize, scroll, switch screens, or move the window between displays to verify attachment and scale.\r\n'
