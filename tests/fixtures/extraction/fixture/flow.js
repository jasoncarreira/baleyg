import { transform, register, recover } from './helpers.js';

/** @param {number} value */
export function direct(value) {
  /* 🧪 café */ return transform(value);
}

export function callbackReference() {
  return register(transform);
}

/** @param {number} value */
export function repeated(value) {
  transform(value);
  return transform(value + 1);
}

/** @param {number} value */
export function branchLoop(value) {
  if (value > 0) {
    if (value > 1) {
      transform(value);
    } else {
      recover();
    }
  } else {
    transform(0);
  }
  for (let index = 0; index < value; index += 1) {
    transform(index);
  }
}

/** @param {number} value */
export function guarded(value) {
  try {
    return transform(value);
  } catch (error) {
    return recover();
  }
}
