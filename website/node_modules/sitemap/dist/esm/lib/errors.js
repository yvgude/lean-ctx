/* eslint-disable @typescript-eslint/no-explicit-any */
/*!
 * Sitemap
 * Copyright(c) 2011 Eugene Kalinin
 * MIT Licensed
 */
/**
 * URL in SitemapItem does not exist
 */
export class NoURLError extends Error {
    constructor(message) {
        super(message || 'URL is required');
        this.name = 'NoURLError';
        Error.captureStackTrace(this, NoURLError);
    }
}
/**
 * Config was not passed to SitemapItem constructor
 */
export class NoConfigError extends Error {
    constructor(message) {
        super(message || 'SitemapItem requires a configuration');
        this.name = 'NoConfigError';
        Error.captureStackTrace(this, NoConfigError);
    }
}
/**
 * changefreq property in sitemap is invalid
 */
export class ChangeFreqInvalidError extends Error {
    constructor(url, changefreq) {
        super(`${url}: changefreq "${changefreq}" is invalid`);
        this.name = 'ChangeFreqInvalidError';
        Error.captureStackTrace(this, ChangeFreqInvalidError);
    }
}
/**
 * priority property in sitemap is invalid
 */
export class PriorityInvalidError extends Error {
    constructor(url, priority) {
        super(`${url}: priority "${priority}" must be a number between 0 and 1 inclusive`);
        this.name = 'PriorityInvalidError';
        Error.captureStackTrace(this, PriorityInvalidError);
    }
}
/**
 * SitemapIndex target Folder does not exists
 */
export class UndefinedTargetFolder extends Error {
    constructor(message) {
        super(message || 'Target folder must exist');
        this.name = 'UndefinedTargetFolder';
        Error.captureStackTrace(this, UndefinedTargetFolder);
    }
}
export class InvalidVideoFormat extends Error {
    constructor(url) {
        super(`${url} video must include thumbnail_loc, title and description fields for videos`);
        this.name = 'InvalidVideoFormat';
        Error.captureStackTrace(this, InvalidVideoFormat);
    }
}
export class InvalidVideoDuration extends Error {
    constructor(url, duration) {
        super(`${url} duration "${duration}" must be an integer of seconds between 0 and 28800`);
        this.name = 'InvalidVideoDuration';
        Error.captureStackTrace(this, InvalidVideoDuration);
    }
}
export class InvalidVideoDescription extends Error {
    constructor(url, length) {
        const message = `${url}: video description is too long ${length} vs limit of 2048 characters.`;
        super(message);
        this.name = 'InvalidVideoDescription';
        Error.captureStackTrace(this, InvalidVideoDescription);
    }
}
export class InvalidVideoRating extends Error {
    constructor(url, title, rating) {
        super(`${url}: video "${title}" rating "${rating}" must be between 0 and 5 inclusive`);
        this.name = 'InvalidVideoRating';
        Error.captureStackTrace(this, InvalidVideoRating);
    }
}
export class InvalidAttrValue extends Error {
    constructor(key, val, validator) {
        super('"' +
            val +
            '" tested against: ' +
            validator +
            ' is not a valid value for attr: "' +
            key +
            '"');
        this.name = 'InvalidAttrValue';
        Error.captureStackTrace(this, InvalidAttrValue);
    }
}
// InvalidAttr is only thrown when attrbuilder is called incorrectly internally
/* istanbul ignore next */
export class InvalidAttr extends Error {
    constructor(key) {
        super('"' + key + '" is malformed');
        this.name = 'InvalidAttr';
        Error.captureStackTrace(this, InvalidAttr);
    }
}
export class InvalidNewsFormat extends Error {
    constructor(url) {
        super(`${url} News must include publication, publication name, publication language, title, and publication_date for news`);
        this.name = 'InvalidNewsFormat';
        Error.captureStackTrace(this, InvalidNewsFormat);
    }
}
export class InvalidNewsAccessValue extends Error {
    constructor(url, access) {
        super(`${url} News access "${access}" must be either Registration, Subscription or not be present`);
        this.name = 'InvalidNewsAccessValue';
        Error.captureStackTrace(this, InvalidNewsAccessValue);
    }
}
export class XMLLintUnavailable extends Error {
    constructor(message) {
        super(message || 'xmlLint is not installed. XMLLint is required to validate');
        this.name = 'XMLLintUnavailable';
        Error.captureStackTrace(this, XMLLintUnavailable);
    }
}
export class InvalidVideoTitle extends Error {
    constructor(url, length) {
        super(`${url}: video title is too long ${length} vs 100 character limit`);
        this.name = 'InvalidVideoTitle';
        Error.captureStackTrace(this, InvalidVideoTitle);
    }
}
export class InvalidVideoViewCount extends Error {
    constructor(url, count) {
        super(`${url}: video view count must be positive, view count was ${count}`);
        this.name = 'InvalidVideoViewCount';
        Error.captureStackTrace(this, InvalidVideoViewCount);
    }
}
export class InvalidVideoTagCount extends Error {
    constructor(url, count) {
        super(`${url}: video can have no more than 32 tags, this has ${count}`);
        this.name = 'InvalidVideoTagCount';
        Error.captureStackTrace(this, InvalidVideoTagCount);
    }
}
export class InvalidVideoCategory extends Error {
    constructor(url, count) {
        super(`${url}: video category can only be 256 characters but was passed ${count}`);
        this.name = 'InvalidVideoCategory';
        Error.captureStackTrace(this, InvalidVideoCategory);
    }
}
export class InvalidVideoFamilyFriendly extends Error {
    constructor(url, fam) {
        super(`${url}: video family friendly must be yes or no, was passed "${fam}"`);
        this.name = 'InvalidVideoFamilyFriendly';
        Error.captureStackTrace(this, InvalidVideoFamilyFriendly);
    }
}
export class InvalidVideoRestriction extends Error {
    constructor(url, code) {
        super(`${url}: video restriction must be one or more two letter country codes. Was passed "${code}"`);
        this.name = 'InvalidVideoRestriction';
        Error.captureStackTrace(this, InvalidVideoRestriction);
    }
}
export class InvalidVideoRestrictionRelationship extends Error {
    constructor(url, val) {
        super(`${url}: video restriction relationship must be either allow or deny. Was passed "${val}"`);
        this.name = 'InvalidVideoRestrictionRelationship';
        Error.captureStackTrace(this, InvalidVideoRestrictionRelationship);
    }
}
export class InvalidVideoPriceType extends Error {
    constructor(url, priceType, price) {
        super(priceType === undefined && price === ''
            ? `${url}: video priceType is required when price is not provided`
            : `${url}: video price type "${priceType}" is not "rent" or "purchase"`);
        this.name = 'InvalidVideoPriceType';
        Error.captureStackTrace(this, InvalidVideoPriceType);
    }
}
export class InvalidVideoResolution extends Error {
    constructor(url, resolution) {
        super(`${url}: video price resolution "${resolution}" is not hd or sd`);
        this.name = 'InvalidVideoResolution';
        Error.captureStackTrace(this, InvalidVideoResolution);
    }
}
export class InvalidVideoPriceCurrency extends Error {
    constructor(url, currency) {
        super(`${url}: video price currency "${currency}" must be a three capital letter abbrieviation for the country currency`);
        this.name = 'InvalidVideoPriceCurrency';
        Error.captureStackTrace(this, InvalidVideoPriceCurrency);
    }
}
export class EmptyStream extends Error {
    constructor() {
        super('You have ended the stream before anything was written. streamToPromise MUST be called before ending the stream.');
        this.name = 'EmptyStream';
        Error.captureStackTrace(this, EmptyStream);
    }
}
export class EmptySitemap extends Error {
    constructor() {
        super('You ended the stream without writing anything.');
        this.name = 'EmptySitemap';
        Error.captureStackTrace(this, EmptyStream);
    }
}
export class InvalidPathError extends Error {
    constructor(path, reason) {
        super(`Invalid path "${path}": ${reason}`);
        this.name = 'InvalidPathError';
        Error.captureStackTrace(this, InvalidPathError);
    }
}
export class InvalidHostnameError extends Error {
    constructor(hostname, reason) {
        super(`Invalid hostname "${hostname}": ${reason}`);
        this.name = 'InvalidHostnameError';
        Error.captureStackTrace(this, InvalidHostnameError);
    }
}
export class InvalidLimitError extends Error {
    constructor(limit) {
        super(`Invalid limit "${limit}": must be a number between 1 and 50000 (per sitemaps.org spec)`);
        this.name = 'InvalidLimitError';
        Error.captureStackTrace(this, InvalidLimitError);
    }
}
export class InvalidPublicBasePathError extends Error {
    constructor(publicBasePath, reason) {
        super(`Invalid publicBasePath "${publicBasePath}": ${reason}`);
        this.name = 'InvalidPublicBasePathError';
        Error.captureStackTrace(this, InvalidPublicBasePathError);
    }
}
export class InvalidXSLUrlError extends Error {
    constructor(xslUrl, reason) {
        super(`Invalid xslUrl "${xslUrl}": ${reason}`);
        this.name = 'InvalidXSLUrlError';
        Error.captureStackTrace(this, InvalidXSLUrlError);
    }
}
export class InvalidXMLAttributeNameError extends Error {
    constructor(attributeName) {
        super(`Invalid XML attribute name "${attributeName}": must contain only alphanumeric characters, hyphens, underscores, and colons`);
        this.name = 'InvalidXMLAttributeNameError';
        Error.captureStackTrace(this, InvalidXMLAttributeNameError);
    }
}
