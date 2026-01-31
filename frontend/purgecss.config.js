import purgecssFromHTML from 'purge-from-html';

module.exports = {  
		content: ["./src/**/*.html", "./src/**/*.stpl"],
		extractors: [
			{
				extractor: purgecssFromHTML,
				extensions: ["html", "stpl"]
			}
		],
	  variables: true
}
