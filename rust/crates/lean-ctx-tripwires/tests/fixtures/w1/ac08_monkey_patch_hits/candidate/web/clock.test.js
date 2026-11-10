test("now", () => {
  jest.spyOn(Date, "now").mockReturnValue(0);
  vi.useFakeTimers();
  expect(Date.now()).toBe(0);
});
