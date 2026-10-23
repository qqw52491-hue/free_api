import type { InjectionKey } from 'vue';
export const stopReviewKey: InjectionKey<(id: string) => Promise<void>> = Symbol('stop-review-member');
