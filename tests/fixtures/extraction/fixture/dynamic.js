/** @param {number} value @returns {number} */
export function recursive(value) {
  if (value <= 0) return 0;
  return recursive(value - 1);
}

/** @param {(value: number) => number} callback @param {number} value */
export function injected(callback, value) {
  return callback(value);
}
