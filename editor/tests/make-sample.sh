#!/bin/bash
# A synthetic "screenshot" (a window with text lines and a form) for screenshots
# of the editor and the benchmarks: make-sample.sh OUT.png [WIDTHxHEIGHT]
set -euo pipefail
out=$1
size=${2:-1600x900}
w=${size%x*}
h=${size#*x}
convert -size "${w}x${h}" gradient:'#dfe6f3-#b9c7e4' \
    -fill '#f6f7fa' -stroke '#8b93a6' -draw "roundrectangle $((w/16)),$((h/10)) $((w*15/16)),$((h*9/10)) 10,10" \
    -fill '#3b4357' -stroke none -draw "rectangle $((w/16)),$((h/10)) $((w*15/16)),$((h/10+h/20))" \
    -fill '#e6e9f0' -draw "rectangle $((w/16+20)),$((h/5)) $((w/4)),$((h*9/10-20))" \
    -fill '#ffffff' -stroke '#c3c9d6' -draw "rectangle $((w*3/10)),$((h*22/100)) $((w*9/10)),$((h*34/100))" \
    -fill '#2a3144' -stroke none -pointsize $((h/30)) -annotate +$((w*31/100))+$((h*29/100)) 'Account: demo@example.com' \
    -pointsize $((h/36)) -annotate +$((w*31/100))+$((h*42/100)) 'The quick brown fox jumps over the lazy dog' \
    -annotate +$((w*31/100))+$((h*48/100)) 'Password reset link sent to your inbox' \
    -annotate +$((w*31/100))+$((h*54/100)) 'Invoice #20418 is due on the 12th of the month' \
    -annotate +$((w*31/100))+$((h*60/100)) 'Token: sk_live_51Hx9d2E4aKj83mPzQ7' \
    -fill '#6858e2' -draw "roundrectangle $((w*3/10)),$((h*70/100)) $((w*3/10+w/9)),$((h*70/100+h/16)) 6,6" \
    -fill white -pointsize $((h/38)) -annotate +$((w*3/10+w/60))+$((h*70/100+h/24)) 'Continue' \
    -fill '#c8cfdd' -draw "roundrectangle $((w*3/10+w/8)),$((h*70/100)) $((w*3/10+w/8+w/9)),$((h*70/100+h/16)) 6,6" \
    "$out"
