import { fileURLToPath } from 'node:url';
export const fixturePath = name => fileURLToPath(new URL(`../../tests/fixtures/selection/${name}`, import.meta.url));
export const extractionPath = name => fileURLToPath(new URL(`../../tests/fixtures/extraction/${name}`, import.meta.url));
export const researchPath = name => fileURLToPath(new URL(`../../docs/research/selection/${name}`, import.meta.url));
export const BUDGET_FILE = fixturePath('outputs/budget.json');
