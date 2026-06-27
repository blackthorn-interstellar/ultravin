CREATE TABLE vpic.pattern (
    id integer NOT NULL,
    vinschemaid integer NOT NULL,
    keys character varying(50) NOT NULL,
    elementid integer NOT NULL,
    attributeid character varying(500) NOT NULL,
    createdon timestamp without time zone,
    updatedon timestamp without time zone,
    keys_regex text GENERATED ALWAYS AS (vpic.sqlwild_to_regex((keys)::text)) STORED
);
