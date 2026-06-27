CREATE TABLE vpic.decodingoutput (
    id integer NOT NULL,
    addedon timestamp without time zone NOT NULL,
    groupname character varying(100),
    variable character varying(100),
    value character varying(500),
    keys character varying(100),
    wmiid integer,
    patternid integer,
    vinschemaid integer,
    elementid integer,
    attributeid character varying(500),
    createdon timestamp without time zone,
    code character varying(50),
    datatype character varying(50),
    decode character varying(50),
    source character varying(50)
);
