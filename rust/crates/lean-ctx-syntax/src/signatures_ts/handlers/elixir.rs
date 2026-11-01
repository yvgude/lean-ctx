// SPDX-License-Identifier: Apache-2.0

use tree_sitter::Node;

use crate::signatures::Signature;

pub(crate) fn elixir_call(node: &Node, name: &str, source: &[u8]) -> Option<Signature> {
    let target = node
        .child_by_field_name("target")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("");
    match target {
        "defmodule" | "defprotocol" => Some(Signature {
            kind: "class",
            name: name.to_string(),
            params: String::new(),
            return_type: String::new(),
            is_async: false,
            is_exported: true,
            indent: 0,
            ..Signature::no_span()
        }),
        "def" | "defmacro" | "defdelegate" | "defguard" => Some(Signature {
            kind: "fn",
            name: name.to_string(),
            params: String::new(),
            return_type: String::new(),
            is_async: false,
            is_exported: true,
            indent: 2,
            ..Signature::no_span()
        }),
        "defp" | "defmacrop" | "defguardp" => Some(Signature {
            kind: "fn",
            name: name.to_string(),
            params: String::new(),
            return_type: String::new(),
            is_async: false,
            is_exported: false,
            indent: 2,
            ..Signature::no_span()
        }),
        _ => None,
    }
}
