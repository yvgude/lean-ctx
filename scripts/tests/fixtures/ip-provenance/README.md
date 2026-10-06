# IP provenance audit fixtures

These tiny files are copied into temporary git repositories by the unittest
suite. They are not dependency notices, legal evidence, or production source.
The tests construct each matrix/ref/symlink adversary in a temporary directory;
the real repository is never mutated.
