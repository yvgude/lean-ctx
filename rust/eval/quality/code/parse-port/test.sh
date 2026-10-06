# SPDX-License-Identifier: Apache-2.0
. ./solution.sh
[ "$(parse_port api.service:6842)" = "6842" ]
[ "$(parse_port localhost:1025)" = "1025" ]
