# common-ancestor-path

Find the common ancestor of 2 or more paths on Windows or Unix

## USAGE

Give it two or more path strings, and it'll do the thing.

```ts
import { commonAncestorPath } from 'common-ancestor-path'
// or:
// const { commonAncestorPath } = require('common-ancestor-path')

// output /a/b
console.log(commonAncestorPath('/a/b/c/d', '/a/b/x/y/z', '/a/b/c/i/j/k'))

// normalizes separators, but NOT cases, since it matters sometimes
console.log(commonAncestorPath('C:\\a\\b\\c', 'C:\\a\\b\\x'))

// no common ancestor on different windows drive letters
// so, this returns null
console.log(commonAncestorPath('c:\\a\\b\\c', 'd:\\d\\e\\f'))
```

## API

`commonAncestorPath(...paths)`

Returns the nearest (deepest) common ancestor path, or `null` if on
different roots on Windows.
