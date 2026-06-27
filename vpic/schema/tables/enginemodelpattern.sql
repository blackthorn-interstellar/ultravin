CREATE TABLE vpic.enginemodelpattern (
    id integer NOT NULL,
    enginemodelid integer NOT NULL,
    elementid integer NOT NULL,
    attributeid character varying(500) NOT NULL,
    createdon timestamp without time zone,
    updatedon timestamp without time zone
);
