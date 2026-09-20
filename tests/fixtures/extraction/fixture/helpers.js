/** @param {number} value */
export function transform(value) {
  return value + 1;
}

/** @param {(value: number) => number} callback */
export function register(callback) {
  return callback;
}

export function recover() {
  return -1;
}
