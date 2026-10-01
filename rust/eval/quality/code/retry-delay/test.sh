. ./solution.sh
[ "$(retry_delay_ms 1)" = "125" ]
[ "$(retry_delay_ms 4)" = "500" ]
[ "$(retry_delay_ms 7)" = "500" ]

