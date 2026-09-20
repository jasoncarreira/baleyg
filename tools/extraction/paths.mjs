import { fileURLToPath } from 'node:url';
export const fixturePath = name => fileURLToPath(new URL(`../../tests/fixtures/extraction/${name}`, import.meta.url));
export const generatedPath = name => fileURLToPath(new URL(`./.generated/${name}`, import.meta.url));
