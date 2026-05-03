Output "assets/leanctx-demo.gif"

Require lean-ctx
Require git

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

Type "lean-ctx --version"
Enter
Sleep 800ms

Type "lean-ctx read rust/src/server/mod.rs -m map"
Enter
Sleep 2s

Type "lean-ctx -c 'git -c color.ui=false log -n 5 --oneline'"
Enter
Sleep 2s
