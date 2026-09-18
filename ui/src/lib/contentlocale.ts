// The country and language YouTube Music localizes to (`gl` / `hl`): what charts, recommendations
// and shelf titles are for. Independent of the interface language on purpose: someone reading the
// app in English may well want Vietnamese charts, and the other way round.
//
// Both lists are what YouTube itself offers in its location and language pickers. Names come from
// `Intl.DisplayNames` in the interface language rather than a translated table of our own.

/** Unset means YouTube's own default, which is what every request asked for before this existed. */
export const DEFAULT_COUNTRY = 'US';
export const DEFAULT_LANGUAGE = 'en';

export const COUNTRIES = [
	'AE', 'AR', 'AT', 'AU', 'AZ', 'BA', 'BD', 'BE', 'BG', 'BH', 'BO', 'BR', 'BY', 'CA', 'CH', 'CL',
	'CO', 'CR', 'CY', 'CZ', 'DE', 'DK', 'DO', 'DZ', 'EC', 'EE', 'EG', 'ES', 'FI', 'FR', 'GB', 'GE',
	'GH', 'GR', 'GT', 'HK', 'HN', 'HR', 'HU', 'ID', 'IE', 'IL', 'IN', 'IQ', 'IS', 'IT', 'JM', 'JO',
	'JP', 'KE', 'KH', 'KR', 'KW', 'KZ', 'LA', 'LB', 'LI', 'LK', 'LT', 'LU', 'LV', 'LY', 'MA', 'ME',
	'MK', 'MT', 'MX', 'MY', 'NG', 'NI', 'NL', 'NO', 'NP', 'NZ', 'OM', 'PA', 'PE', 'PG', 'PH', 'PK',
	'PL', 'PR', 'PT', 'PY', 'QA', 'RO', 'RS', 'RU', 'SA', 'SE', 'SG', 'SI', 'SK', 'SN', 'SV', 'TH',
	'TN', 'TR', 'TW', 'TZ', 'UA', 'UG', 'US', 'UY', 'VE', 'VN', 'YE', 'ZA', 'ZW'
];

export const LANGUAGES = [
	'af', 'am', 'ar', 'as', 'az', 'be', 'bg', 'bn', 'bs', 'ca', 'cs', 'da', 'de', 'el', 'en', 'en-GB',
	'en-IN', 'es', 'es-419', 'es-US', 'et', 'eu', 'fa', 'fi', 'fil', 'fr', 'fr-CA', 'gl', 'gu', 'hi',
	'hr', 'hu', 'hy', 'id', 'is', 'it', 'iw', 'ja', 'ka', 'kk', 'km', 'kn', 'ko', 'ky', 'lo', 'lt',
	'lv', 'mk', 'ml', 'mn', 'mr', 'ms', 'my', 'ne', 'nl', 'no', 'or', 'pa', 'pl', 'pt', 'pt-PT', 'ro',
	'ru', 'si', 'sk', 'sl', 'sq', 'sr', 'sr-Latn', 'sv', 'sw', 'ta', 'te', 'th', 'tr', 'uk', 'ur', 'uz',
	'vi', 'zh-CN', 'zh-HK', 'zh-TW', 'zu'
];

function displayNames(ui: string, type: 'region' | 'language'): Intl.DisplayNames | null {
	try {
		return new Intl.DisplayNames([ui, 'en'], { type });
	} catch {
		return null;
	}
}

/** Countries as `{ code, name }`, sorted by name in the interface language. */
export function countryOptions(ui: string): { code: string; name: string }[] {
	const names = displayNames(ui, 'region');
	return COUNTRIES.map((code) => ({ code, name: names?.of(code) ?? code })).sort((a, b) =>
		a.name.localeCompare(b.name, ui)
	);
}

/** Languages as `{ code, name }`, each named in the interface language, sorted by that name. */
export function languageOptions(ui: string): { code: string; name: string }[] {
	const names = displayNames(ui, 'language');
	// `iw` is YouTube's code for Hebrew, a tag Intl knows as `he`.
	const tag = (code: string) => (code === 'iw' ? 'he' : code);
	return LANGUAGES.map((code) => ({ code, name: names?.of(tag(code)) ?? code })).sort((a, b) =>
		a.name.localeCompare(b.name, ui)
	);
}
