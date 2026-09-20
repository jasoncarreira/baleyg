function actual() { return 1; }
function key() { return 'method'; }
function a() { return 1; }
function b() { return 2; }
function sink(callback) { return callback; }
const obj = { get f() { return actual; }, method() { return actual(); } };
obj.f();
class C { [key()]() { actual(); } }
export function branching(x) {
  x ? a() : b();
  x && a();
  for (a(); b(); a()) actual();
}
export function callbacks() {
  sink((() => actual()));
  sink(obj.method);
}
