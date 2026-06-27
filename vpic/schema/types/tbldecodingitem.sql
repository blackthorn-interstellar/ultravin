CREATE TYPE vpic."tblDecodingItem" AS (
	"Id" integer,
	"DecodingId" integer,
	"CreatedOn" timestamp without time zone,
	"PatternId" integer,
	"Keys" character varying(50),
	"VinSchemaId" integer,
	"WmiId" integer,
	"ElementId" integer,
	"AttributeId" character varying(500),
	"Value" character varying(500),
	"Source" character varying(50),
	"Priority" integer,
	"TobeQCed" boolean
);
