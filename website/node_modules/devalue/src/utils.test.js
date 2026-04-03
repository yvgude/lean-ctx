import * as assert from 'uvu/assert';
import { suite } from 'uvu';
import { valid_array_indices } from './utils.js';

const test = suite('valid_array_indices');

test('returns all indices for a normal dense array', () => {
	const arr = ['a', 'b', 'c'];
	assert.equal(valid_array_indices(arr), ['0', '1', '2']);
});

test('returns empty array for an empty array', () => {
	assert.equal(valid_array_indices([]), []);
});

test('returns populated indices for a sparse array', () => {
	const arr = [, 'b', ,];
	assert.equal(valid_array_indices(arr), ['1']);
});

test('strips non-numeric properties from a dense array', () => {
	const arr = ['a', 'b'];
	arr.foo = 'x';
	arr.bar = 42;
	assert.equal(valid_array_indices(arr), ['0', '1']);
});

test('strips non-numeric properties from a very sparse array', () => {
	const arr = [];
	arr[1_000_000] = 'x';
	arr.foo = 'should be ignored';
	assert.equal(valid_array_indices(arr), ['1000000']);
});

test('returns empty array when only non-numeric properties exist', () => {
	const arr = [];
	arr.foo = 'x';
	arr.bar = 42;
	assert.equal(valid_array_indices(arr), []);
});

test('handles multiple non-numeric properties after indices', () => {
	const arr = [1, 2, 3];
	arr.a = 'x';
	arr.b = 'y';
	arr.c = 'z';
	assert.equal(valid_array_indices(arr), ['0', '1', '2']);
});

test('handles a single-element array with non-numeric property', () => {
	const arr = ['only'];
	arr.extra = true;
	assert.equal(valid_array_indices(arr), ['0']);
});

test('handles array properties pretending to be indices', () => {
	const arr = ['a', 'b'];
	arr[-1] = 'negative index';
	arr[2**32 - 1] = 'too large index';
	assert.equal(valid_array_indices(arr), ['0', '1']);
})

test.run();
