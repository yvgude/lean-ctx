Output "assets/leanctx-gain.gif"

Require lean-ctx

Set Theme "GitHub Dark"
Set FontSize 24
Set Width 1200
Set Height 700
Set Framerate 30
Set Padding 24
Set BorderRadius 10
Set WindowBar "ColorfulRight"
Set WindowBarSize 36
Set TypingSpeed 45ms
Set LoopOffset 10%

Set Shell "bash"

Type "export PS1='leanctx$ '" Enter
Sleep 200ms

Type "lean-ctx gain --live" Enter
Sleep 3s
Ctrl+C
Sleep 500ms

Type "lean-ctx gain --daily" Enter
Sleep 2s
